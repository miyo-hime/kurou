# kurou 烏

![version](https://img.shields.io/badge/v0.6.0-orange)
![built with rust](https://img.shields.io/badge/Rust-CE412B?logo=rust&logoColor=white)
![mcp](https://img.shields.io/badge/MCP-rmcp-purple)

> 烏 crow on the wire / 苦労 toil (we wrote it in rust). a small, deliberately
> dim window into one discord server.

a tiny rust MCP server that lets an assistant peek at a discord server and, when it
has something to say, say it. a small, focused tool surface and nothing more. by
default it talks to discord over the REST api only - **no gateway, no websocket** - so
it can read, it can post, but it can never be pinged, mentioned, or summoned by anyone
in the server. if you opt into gateway mode, it can appear online and collect a tiny
mention inbox without ever auto-replying.

this is a personal tool. it started as a ~5-tool rewrite of the ~50-tool
[SaseQ/discord-mcp](https://github.com/SaseQ/discord-mcp) (java/spring/JDA), trimmed
down to just the read-window-plus-voice that one assistant actually needed. if you
stumbled in here, hi - it works, but it was built for an audience of one. use at your
own risk.

## what you get

- streamable MCP over **stdio** (local) or **HTTP** (hosted)
- a focused read-window: list servers/channels/threads, read messages (anchored), fetch one message, read pins, deep-sweep a channel by author/mention/text, plus check/mark a mention inbox
- voice when it has something to say: send a message, find a user id
- optional read-only **secondary servers** on a separate observer bot - watch more, post in none
- compact message blocks built for an LLM to read, author ids inline, reply context inline
- bearer auth + a tiny PKCE OAuth shim for hosted HTTP
- structurally un-summonable by default: REST only, the gateway is never opened unless you ask for it
- optional gateway presence/mention mode for online status and a pull-based mention inbox
- one small static binary, rustls (no openssl), bundled sqlite when mention mode is enabled

it can post messages. that's a real action in a real server. `send_message` exists
because the whole point was letting the crow drop the occasional remark, but it's an
LLM with a discord account and no sense of social consequence, so: indoor voice.

## quick start

you need a **discord bot token** and the **guild (server) id** you want the window
pointed at. the bot has to actually be a member of that server. then:

```powershell
$env:DISCORD_TOKEN = "your_bot_token_here"
$env:DISCORD_GUILD_ID = "your_server_id_here"
.\kurou.exe --transport stdio
```

linux/macos:

```bash
DISCORD_TOKEN="your_bot_token_here" DISCORD_GUILD_ID="your_server_id_here" ./kurou --transport stdio
```

stdio is the easy path - your MCP client launches the process and talks over
stdin/stdout. minimal client shape:

```json
{
  "mcpServers": {
    "kurou": {
      "command": "/path/to/kurou",
      "args": ["--transport", "stdio"],
      "env": {
        "DISCORD_TOKEN": "your_bot_token_here",
        "DISCORD_GUILD_ID": "your_server_id_here"
      }
    }
  }
}
```

note: kurou reads its config straight from the process environment (clap/env). it does
**not** load a `.env` file on its own. either export the vars first or put them in your
client's `env` block above.

## the tools

`guild_id` defaults to `DISCORD_GUILD_ID` on every tool that takes it, so you usually
leave it out. snowflake ids come back as strings, because they outlive js number
precision and nobody wants that bug.

| tool | mutates? | what it does |
|---|---:|---|
| `list_servers` | no | the guilds the crow can read, each tagged `primary` (writable) or `readonly` (watch-only, separate bot) |
| `get_server_info` | no | guild name, id, member count, description |
| `list_channels` | no | every channel with id, name, kind (Text/Voice/Category/Forum/...), and topic |
| `list_threads` | no | the active (non-archived) threads in a guild; a thread id works anywhere a channel id does |
| `read_messages` | no | messages from a channel or thread as compact blocks, newest first; reactions/attachments/stickers/embeds/reply-context when present. anchor with `around`/`before`/`after` a message id to read a specific slice. `limit` clamps 1-100 (default 50) |
| `get_message` | no | one message by id, same compact block |
| `get_pinned` | no | a channel's pinned messages (discord's own pins endpoint) |
| `scan_channel` | no | deep-sweep a channel by paging backward, filtering by author / mention / text (case-insensitive). bounded by `max_pages`; reports `reached_cap` + `oldest_scanned_id` so you can continue. our own search, since discord's is closed to bots |
| `check_mentions` | no | reads the collected mention inbox when `GATEWAY_MODE=mentions`. `limit` clamps 1-100 (default 20) |
| `mark_mentions_seen` | yes-ish | marks mention inbox rows as seen. pass `ids`, or omit them to mark all unseen rows |
| `send_message` | **yes** | posts to a channel. up to discord's 2000 chars, plus optional file attachments. this changes the server |
| `get_user_id_by_name` | no | prefix-search guild members by username/nick, returns ids, display names, nicknames, and `<@id>` mention strings. `limit` 1-100 (default 10) |

`list_channels` returns *all* channels, not just text ones - a read window is more
useful seeing the whole layout, and the `kind` field tells you what each one is.
`get_user_id_by_name` is prefix search (that's what discord's REST endpoint gives
you), so it's good for finding people, not for fuzzy magic. `scan_channel` is the one
heavy read - it makes several API calls, so it's a separate tool rather than a flag,
and the page cap keeps it honest.

## read-only secondary servers

the crow can watch more than one server. the primary (`DISCORD_GUILD_ID`) is the only
place it's ever allowed to speak; any guilds you list in `READONLY_GUILDS` are
watch-only. those secondaries ride a **separate observer bot** (`READONLY_DISCORD_TOKEN`)
- a different discord application, so it structurally *cannot* post as your primary bot
there. read-only stops being a code check and becomes a discord-level fact.

```env
DISCORD_TOKEN=primary_bot_token
DISCORD_GUILD_ID=primary_guild_id
READONLY_GUILDS=other_guild_a,other_guild_b
READONLY_DISCORD_TOKEN=observer_bot_token
```

routing is automatic: tools that take a `guild_id` pick the right bot from the guild,
and channel-scoped reads (`read_messages`, `get_message`, `get_pinned`, `scan_channel`)
resolve the bot from the channel for you. `send_message` resolves the target channel's
guild and refuses anything that isn't the primary. when `READONLY_GUILDS` is empty none
of this is active and there's no overhead. call `list_servers` to see which guild is
which.

## attachments

`send_message` carries files three ways, cheapest first:

- `attachment_urls` - already-hosted http(s) links. the crow fetches them itself.
- `attachment_refs` - refs from the upload endpoint (see below). the way to attach a
  local file without the bytes passing through the assistant's context.
- `attachments_inline` - `{ filename, data_base64 }`. last resort; the bytes ride in
  the tool call and cost tokens, so reach for a ref or url first.

up to 10 files per message, 25MiB each. content may be empty if at least one file is
attached.

### the upload endpoint

hosted http mode also exposes `POST /upload` behind the same bearer auth:

```bash
curl -fsS -X POST "https://kurou.example.com/upload?filename=proof.png" \
  -H "Authorization: Bearer <token>" \
  --data-binary @proof.png
# -> {"ref":"03392...","filename":"proof.png","size":81234,"expires_in_secs":600}
```

the bytes land in a short-lived in-memory store (~10 min ttl, single-use). hand the
`ref` to `send_message` as `attachment_refs` and the crow attaches what it already
holds. the companion plugin in [`companion/kurou-upload`](companion/kurou-upload) wraps
this into a `/kurou-upload <path>` command so the assistant never courier-carries the
bytes herself.

if you front the crow with nginx, give `/upload` room to breathe:
`client_max_body_size 25m;` (the default 1m will bounce real files).

## gateway mode

default is still fully REST-only:

```env
GATEWAY_MODE=off
```

if you want Koma to appear online but remain uninterruptible:

```env
GATEWAY_MODE=presence
```

if you also want the little pull-based inbox for `koma` sightings:

```env
GATEWAY_MODE=mentions
MENTION_KEYWORDS=koma
MENTION_DB_PATH=/var/lib/kurou/mentions.sqlite3
```

`mentions` mode opens the Discord gateway with message events and message content
intent. the bot needs that privileged intent enabled in Discord's developer portal
if you want keyword matches like `koma`; direct bot mentions can still be detected
from Discord's mention metadata when Discord sends it. the inbox is SQLite via
`rusqlite`'s bundled SQLite, so the systemd service does not need a separate sqlite
package. it is just the binary and a db file.

systemd-friendly storage:

```ini
StateDirectory=kurou
Environment=MENTION_DB_PATH=/var/lib/kurou/mentions.sqlite3
```

## hosted http

HTTP mode serves streamable MCP at `/mcp`:

```bash
DISCORD_TOKEN="..." DISCORD_GUILD_ID="..." \
  ./kurou --transport http --host 0.0.0.0 --port 3000
```

for anything reachable from outside localhost, set bearer auth - otherwise `/mcp` is
wide open and anyone who finds it gets your crow:

```bash
export DISCORD_TOKEN="your_bot_token_here"
export DISCORD_GUILD_ID="your_server_id_here"
export AUTH_TOKENS="koma:paste-a-random-token-here"
export PUBLIC_BASE_URL="https://kurou.example.com"
export ALLOWED_HOSTS="kurou.example.com"
./kurou --transport http --host 0.0.0.0 --port 3000
```

clients that take raw headers connect to `https://kurou.example.com/mcp` with:

```http
Authorization: Bearer paste-a-random-token-here
```

the streamable transport replies on the POST itself as server-sent events, so your
client has to send `Accept: application/json, text/event-stream`. miss the
`text/event-stream` part and you get a 406. don't ask how long that one took to notice.

### oauth clients

some MCP clients want OAuth discovery instead of a hand-set bearer token. when
`AUTH_TOKENS` is set, HTTP mode exposes a small shim:

| route | purpose |
|---|---|
| `/.well-known/oauth-authorization-server` | where `/authorize` and `/token` live |
| `/.well-known/oauth-protected-resource` | tells the client this server uses bearer auth |
| `/authorize` | tiny approval page |
| `/token` | PKCE authorization-code exchange |
| `/mcp` | the protected endpoint |
| `/upload` | stash a file, get a ref for `send_message` (same bearer) |

it issues one configured bearer token. it is compatibility glue, not an identity
provider - anyone who can reach and approve `/authorize` can mint that token, so put
it behind your reverse proxy if it's exposed. oauth cosplay still needs a chaperone.

```env
DISCORD_TOKEN=your_bot_token_here
DISCORD_GUILD_ID=your_server_id_here
AUTH_TOKENS=koma:paste-a-random-token-here
OAUTH_TOKEN_LABEL=koma
PUBLIC_BASE_URL=https://kurou.example.com
ALLOWED_HOSTS=kurou.example.com
HOST=0.0.0.0
PORT=3000
TRANSPORT=http
```

## config

| env var | cli flag | default | notes |
|---|---|---|---|
| `DISCORD_TOKEN` | `--discord-token` | none | required; the bot token |
| `DISCORD_GUILD_ID` | `--discord-guild-id` | none | default + primary guild; the only place `send_message` may post |
| `READONLY_GUILDS` | `--readonly-guild` | empty | comma-separated guild ids the crow may read but never post in |
| `READONLY_DISCORD_TOKEN` | `--readonly-token` | none | observer bot token for the read-only guilds; required when `READONLY_GUILDS` is set |
| `TRANSPORT` | `--transport` | `stdio` | `stdio` or `http` |
| `HOST` | `--host` | `127.0.0.1` | use `0.0.0.0` for hosted http |
| `PORT` | `--port` | `3000` | http listen port |
| `ALLOWED_HOSTS` | `--allowed-host` | local host values | comma-separated accepted `Host` headers |
| `ALLOWED_ORIGINS` | `--allowed-origin` | empty | comma-separated browser origins |
| `AUTH_TOKENS` | `--auth-token` | empty | comma-separated `label:token` for http bearer auth |
| `OAUTH_TOKEN_LABEL` | `--oauth-token-label` | first auth token | which `AUTH_TOKENS` label the oauth shim issues |
| `PUBLIC_BASE_URL` | `--public-base-url` | `http://127.0.0.1:3000` | external base url used in auth metadata |
| `GATEWAY_MODE` | `--gateway-mode` | `off` | `off`, `presence`, or `mentions` |
| `MENTION_DB_PATH` | `--mention-db-path` | `mentions.sqlite3` | sqlite file used by `GATEWAY_MODE=mentions` |
| `MENTION_KEYWORDS` | `--mention-keyword` | `koma` | comma-separated keyword list for the mention inbox |
| `RUST_LOG` | n/a | unset | try `kurou=info` when something's quiet |

## the un-summonable thing

most discord bots open a gateway websocket so they can receive events in real time.
kurou does not do that unless `GATEWAY_MODE` says so. in `off` mode, it makes plain
REST calls and goes back to sleep. the upside isn't just a smaller binary - it means
the assistant behind it can't be mentioned, replied at, or dragged into a
conversation by server members. it's a one-way mirror on purpose.

`presence` mode opens the gateway just long enough to stay connected and show online.
`mentions` mode also listens for new messages and stores matches in SQLite for later
inspection. neither mode replies to Discord events. Koma still checks the tray only
when she chooses.

(serenity still drags `tokio-tungstenite` into the build through an unrelated feature,
but the `gateway` feature is off, so no shard or event loop ever compiles in. the ws
crate is dead weight, not an open socket. lto strips most of it.)

## build from source

needs a recent stable rust. release binaries are static
`aarch64-unknown-linux-musl`; for anything else, build it yourself:

```bash
cargo build --release
```

cross-compile a static musl binary for an aarch64 box (a raspberry pi, say). we use
[`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild) because it needs no
docker and we're rustls-only so there's no C to link:

```bash
rustup target add aarch64-unknown-linux-musl
cargo zigbuild --release --target aarch64-unknown-linux-musl
```

### development

```bash
cargo fmt --check
cargo clippy --all-targets
cargo check
cargo build
```

## stack

```text
MCP client  <->  kurou  <->  discord REST api
```

- rust + tokio
- `rmcp` for MCP (pinned `=1.6.0` on purpose - the caret floats to 1.7 and breaks the schemars gating)
- `serenity` for typed discord models, REST calls, and optional gateway presence
- `rusqlite` with bundled SQLite for the optional mention inbox
- `axum` for the http transport, rustls all the way down
- `serde` / `schemars` for payloads and tool schemas

small binary, narrow purpose, quiet by design. a crow on a wire, watching the cat.
