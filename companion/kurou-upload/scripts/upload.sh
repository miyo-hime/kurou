#!/usr/bin/env bash
# the bytes go machine-to-machine. koma only ever sees the little ref that comes back.
set -euo pipefail

base="${KUROU_UPLOAD_URL:-https://kurou.kurobox.me}"
token="${KUROU_UPLOAD_TOKEN:-}"
file="${1:-}"

if [[ -z "$file" ]]; then
  echo "usage: kurou-upload <path-to-file>" >&2
  exit 1
fi
if [[ ! -f "$file" ]]; then
  echo "no file here: $file" >&2
  exit 1
fi
if [[ -z "$token" ]]; then
  echo "set KUROU_UPLOAD_TOKEN to the crow's bearer first (same token koma's mcp uses)" >&2
  exit 1
fi

urlencode() {
  local s="$1" out="" c i
  for (( i=0; i<${#s}; i++ )); do
    c="${s:i:1}"
    case "$c" in
      [a-zA-Z0-9._~-]) out+="$c" ;;
      *) printf -v c '%%%02X' "'$c"; out+="$c" ;;
    esac
  done
  printf '%s' "$out"
}

# audio files get measured on the way out so the crow can send them as discord
# voice messages: duration from ffprobe, a peak envelope (max 256 points, one
# per 100ms) sketched by ffmpeg. anything else uploads exactly as before.
meta_args=()
case "${file,,}" in
  *.ogg|*.opus|*.mp3|*.wav|*.flac|*.m4a)
    if command -v ffprobe >/dev/null && command -v ffmpeg >/dev/null && command -v xxd >/dev/null; then
      dur="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$file" 2>/dev/null || true)"
      if [[ -n "$dur" && "$dur" != "N/A" ]]; then
        waveform="$(ffmpeg -v error -i "$file" -ac 1 -ar 4000 -f u8 - 2>/dev/null \
          | od -An -v -tu1 | tr -s ' \n' '\n\n' | sed '/^$/d' \
          | awk -v dur="$dur" '
              { v = $1 - 128; if (v < 0) v = -v; v *= 2; if (v > 255) v = 255; a[NR] = v }
              END {
                if (NR == 0) exit 1
                bins = int(dur * 10); if (bins > 256) bins = 256; if (bins < 1) bins = 1
                for (i = 1; i <= NR; i++) { b = int((i - 1) * bins / NR); if (a[i] > peak[b]) peak[b] = a[i] }
                for (b = 0; b < bins; b++) printf "%02x", peak[b]
              }' \
          | xxd -r -p | base64 -w0 || true)"
        if [[ -n "$waveform" ]]; then
          meta_args=(-H "X-Kurou-Duration: ${dur}" -H "X-Kurou-Waveform: ${waveform}")
        fi
      fi
    fi
    ;;
esac

name="$(urlencode "$(basename "$file")")"
curl -fsS -X POST "${base%/}/upload?filename=${name}" \
  -H "Authorization: Bearer ${token}" \
  "${meta_args[@]}" \
  --data-binary "@${file}"
echo ""
