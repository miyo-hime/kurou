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
    expires_at: Instant,
}

impl UploadStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    pub fn put(&self, filename: String, data: Vec<u8>) -> String {
        let id = Uuid::new_v4().simple().to_string();
        let mut map = self.inner.lock().expect("upload store mutex poisoned");
        sweep(&mut map);
        map.insert(
            id.clone(),
            Stored {
                filename,
                data,
                expires_at: Instant::now() + self.ttl,
            },
        );
        id
    }

    // one upload, one send. taking removes it so a ref can't be replayed.
    pub fn take(&self, id: &str) -> Option<(String, Vec<u8>)> {
        let mut map = self.inner.lock().expect("upload store mutex poisoned");
        sweep(&mut map);
        let stored = map.remove(id)?;
        if stored.expires_at <= Instant::now() {
            return None;
        }
        Some((stored.filename, stored.data))
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

    let size = body.len();
    let id = store.put(filename.clone(), body.to_vec());
    let ttl_secs = store.ttl.as_secs();

    tracing::info!(%id, filename, size, "stashed upload for send_message");
    (
        StatusCode::OK,
        Json(json!({
            "ref": id,
            "filename": filename,
            "size": size,
            "expires_in_secs": ttl_secs,
        })),
    )
        .into_response()
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
