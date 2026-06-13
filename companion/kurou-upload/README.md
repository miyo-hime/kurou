# kurou-upload

> the crow's beak. hands a local file to kurou so koma can attach it without
> base64-ing megabytes through her own context window.

## why this exists

kurou (the crow) runs on kurobox. koma runs on a different box. the only wire
between them is the mcp endpoint, so the naive way to attach a local file is to
base64 it into the `send_message` tool call - which means the bytes sit in koma's
context and cost real tokens every time.

this skips that. the file goes straight from koma's disk to the crow's `/upload`
endpoint over raw http, lands in a short-lived in-memory store, and koma gets back
a tiny `ref` string. she passes that ref to `send_message` (`attachment_refs`) and
the crow attaches the bytes it already has. token cost: the length of a uuid.

## install

drop this folder where your harness loads plugins (or symlink it), e.g.

```bash
ln -s "$(pwd)/companion/kurou-upload" ~/.claude/plugins/kurou-upload
```

then set two env vars in your shell profile:

| var | default | what |
| --- | --- | --- |
| `KUROU_UPLOAD_TOKEN` | (required) | the crow's bearer - same token koma's mcp uses |
| `KUROU_UPLOAD_URL` | `https://kurou.kurobox.me` | crow base url, no trailing `/mcp` |

## use

```
/kurou-upload D:\screenshots\proof.png
```

prints something like:

```json
{"ref":"03392cfae12546c5bae65c3a0fdd07e1","filename":"proof.png","size":81234,"expires_in_secs":600}
```

then koma calls `send_message(channel_id, content, attachment_refs: ["03392..."])`.

the ref is single-use and expires in ~10 minutes. upload, then send.

## the script alone

no plugin needed if you just want the curl:

```bash
KUROU_UPLOAD_TOKEN=... bash scripts/upload.sh ./some-file.png
```
