# kurou 烏

![version](https://img.shields.io/badge/v0.3.0-orange)
![built with rust](https://img.shields.io/badge/Rust-CE412B?logo=rust&logoColor=white)
![mcp](https://img.shields.io/badge/MCP-rmcp-purple)

> 烏 crow on the wire / 苦労 toil (we wrote it in rust). a small, deliberately
> dim window into one discord server.

a tiny rust MCP server that lets an assistant peek at a discord server and, when it
has something to say, say it. five tools, no more. it talks to discord over the REST
api only - **no gateway, no websocket** - so it can read, it can post, but it can
never be pinged, mentioned, or summoned by anyone in the server. it watches; it
doesn't get watched back.

this is a personal tool. it started as a ~5-tool rewrite of the ~50-tool
[SaseQ/discord-mcp](https://github.com/SaseQ/discord-mcp) (java/spring/JDA), trimmed
down to just the read-window-plus-voice that one assistant actually needed. if you
stumbled in here, hi - it works, but it was built for an audience of one. use at your
own risk.

## what you get

- streamable MCP over **stdio** (local) or **HTTP** (hosted)
- five tools: read server info, list channels, read messages, send a message, find a user id
- compact message blocks built for an LLM to read, author ids inline
- bearer auth + a tiny PKCE OAuth shim for hosted HTTP
- structurally un-summonable: REST only, the gateway is never opened
- one small static binary, rustls (no openssl), no runtime to install

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
| `get_server_info` | no | guild name, id, member count, description |
| `list_channels` | no | every channel with id, name, kind (Text/Voice/Category/Forum/...), and topic |
| `read_messages` | no | recent messages from a channel as compact blocks, newest first, author ids inline; reactions/attachments/stickers/embeds when present. `limit` clamps 1-100 (default 50) |
| `send_message` | **yes** | posts to a channel. up to discord's 2000 chars. this changes the server |
| `get_user_id_by_name` | no | prefix-search guild members by username/nick, returns matching user ids. `limit` 1-100 (default 10) |

`list_channels` returns *all* channels, not just text ones - a read window is more
useful seeing the whole layout, and the `kind` field tells you what each one is.
`get_user_id_by_name` is prefix search (that's what discord's REST endpoint gives
you), so it's good for finding people, not for fuzzy magic.

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
| `DISCORD_GUILD_ID` | `--discord-guild-id` | none | default guild for tools that take `guild_id` |
| `TRANSPORT` | `--transport` | `stdio` | `stdio` or `http` |
| `HOST` | `--host` | `127.0.0.1` | use `0.0.0.0` for hosted http |
| `PORT` | `--port` | `3000` | http listen port |
| `ALLOWED_HOSTS` | `--allowed-host` | local host values | comma-separated accepted `Host` headers |
| `ALLOWED_ORIGINS` | `--allowed-origin` | empty | comma-separated browser origins |
| `AUTH_TOKENS` | `--auth-token` | empty | comma-separated `label:token` for http bearer auth |
| `OAUTH_TOKEN_LABEL` | `--oauth-token-label` | first auth token | which `AUTH_TOKENS` label the oauth shim issues |
| `PUBLIC_BASE_URL` | `--public-base-url` | `http://127.0.0.1:3000` | external base url used in auth metadata |
| `RUST_LOG` | n/a | unset | try `kurou=info` when something's quiet |

## the un-summonable thing

most discord bots open a gateway websocket so they can receive events in real time.
kurou never does. it makes plain REST calls and goes back to sleep. the upside isn't
just a smaller binary - it means the assistant behind it can't be mentioned, replied
at, or dragged into a conversation by server members. it's a one-way mirror on
purpose. the tradeoff is no live events and no group chat; if you want those, this is
the wrong tool and you want the gateway you just turned off.

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
- `serenity` (`http` feature only, gateway off) for typed discord models
- `axum` for the http transport, rustls all the way down
- `serde` / `schemars` for payloads and tool schemas

small binary, narrow purpose, quiet by design. a crow on a wire, watching the cat.
