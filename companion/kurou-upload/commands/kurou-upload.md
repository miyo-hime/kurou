---
description: upload a local file to the kurou crow, get back an attachment ref for send_message
argument-hint: <path-to-file>
allowed-tools: Bash(bash:*)
---

upload result for `$ARGUMENTS`:

!`bash "${CLAUDE_PLUGIN_ROOT}/scripts/upload.sh" "$ARGUMENTS"`

if that printed a `ref`, attach it by calling kurou's `send_message` with
`attachment_refs: ["<ref>"]`. the ref expires in ~10 minutes, so don't dawdle.
