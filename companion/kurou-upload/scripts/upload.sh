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

name="$(urlencode "$(basename "$file")")"
curl -fsS -X POST "${base%/}/upload?filename=${name}" \
  -H "Authorization: Bearer ${token}" \
  --data-binary "@${file}"
echo ""
