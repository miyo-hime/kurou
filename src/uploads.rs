use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json,
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::Engine;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

// discord eats 25MiB from un-boosted servers. anything past that the crow rejects
// before it ever reaches discord, so koma gets a clean error instead of a 413 from
// the other side of the world.
pub const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

#[derive(Clone)]
pub struct UploadStore {
    inner: Arc<Mutex<HashMap<String, Stored>>>,
    ttl: Duration,
}

struct Stored {
    filename: String,
    data: Vec<u8>,
    duration_secs: Option<f64>,
    waveform: Option<String>,
    expires_at: Instant,
}

pub struct Upload {
    pub filename: String,
    pub data: Vec<u8>,
    pub duration_secs: Option<f64>,
    pub waveform: Option<String>,
}

impl UploadStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    pub fn put(&self, filename: String, data: Vec<u8>, duration_secs: Option<f64>, waveform: Option<String>) -> String {
        let id = Uuid::new_v4().simple().to_string();
        let mut map = self.inner.lock().expect("upload store mutex poisoned");
        sweep(&mut map);
        map.insert(
            id.clone(),
            Stored {
                filename,
                data,
                duration_secs,
                waveform,
                expires_at: Instant::now() + self.ttl,
            },
        );
        id
    }

    // one upload, one send. taking removes it so a ref can't be replayed.
    pub fn take(&self, id: &str) -> Option<Upload> {
        let mut map = self.inner.lock().expect("upload store mutex poisoned");
        sweep(&mut map);
        let stored = map.remove(id)?;
        if stored.expires_at <= Instant::now() {
            return None;
        }
        Some(Upload {
            filename: stored.filename,
            data: stored.data,
            duration_secs: stored.duration_secs,
            waveform: stored.waveform,
        })
    }
}

fn sweep(map: &mut HashMap<String, Stored>) {
    let now = Instant::now();
    map.retain(|_, stored| stored.expires_at > now);
}

// no peeking at strangers' bytes in the logs
impl std::fmt::Debug for UploadStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pending = self.inner.lock().map(|map| map.len()).unwrap_or(0);
        f.debug_struct("UploadStore")
            .field("pending", &pending)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize)]
pub struct UploadParams {
    filename: String,
}

pub async fn upload_handler(
    State(store): State<UploadStore>,
    Query(params): Query<UploadParams>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let filename = sanitize_filename(&params.filename);
    if filename.is_empty() {
        return error(StatusCode::BAD_REQUEST, "filename query param is required");
    }
    if body.is_empty() {
        return error(StatusCode::BAD_REQUEST, "upload body is empty");
    }
    if body.len() > MAX_UPLOAD_BYTES {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!(
                "upload is {} bytes; the crow's cap is {MAX_UPLOAD_BYTES}",
                body.len()
            ),
        );
    }
    let (duration_secs, waveform) = match voice_meta(&headers) {
        Ok(meta) => meta,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };

    let size = body.len();
    let voice_ready = duration_secs.is_some() && waveform.is_some();
    let id = store.put(filename.clone(), body.to_vec(), duration_secs, waveform);
    let ttl_secs = store.ttl.as_secs();

    tracing::info!(%id, filename, size, voice_ready, "stashed upload for send_message");
    (
        StatusCode::OK,
        Json(json!({
            "ref": id,
            "filename": filename,
            "size": size,
            "expires_in_secs": ttl_secs,
            "voice_ready": voice_ready,
        })),
    )
        .into_response()
}

// the companion measures audio at upload time (ffprobe + ffmpeg live on the
// uploader's box, not the crow's) and ships the results as headers.
fn voice_meta(headers: &axum::http::HeaderMap) -> Result<(Option<f64>, Option<String>), String> {
    let duration_secs = match headers.get("x-kurou-duration") {
        None => None,
        Some(value) => {
            let parsed = value.to_str().ok().and_then(|v| v.trim().parse::<f64>().ok());
            match parsed {
                Some(secs) if secs > 0.0 => Some(secs),
                _ => return Err("x-kurou-duration must be a positive number of seconds".to_string()),
            }
        }
    };
    let waveform = match headers.get("x-kurou-waveform") {
        None => None,
        Some(value) => {
            let raw = value.to_str().map_err(|_| "x-kurou-waveform is not ascii".to_string())?.trim().to_string();
            let decoded = base64::engine::general_purpose::STANDARD.decode(&raw).map_err(|_| "x-kurou-waveform is not valid base64".to_string())?;
            if decoded.is_empty() || decoded.len() > 256 {
                return Err(format!("x-kurou-waveform decodes to {} bytes; discord wants 1-256 datapoints", decoded.len()));
            }
            Some(raw)
        }
    };
    Ok((duration_secs, waveform))
}

// strip any path the caller's basename logic missed. the crow only ever wants a
// leaf name to hand discord.
fn sanitize_filename(raw: &str) -> String {
    raw.rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}
