# Aurabot Rust

Aurabot Rust is a standalone, single-process QQ companion server. It does not load DeepSeek Harness. OneBot v11 and the QQ Open Platform feed a shared, channel-aware conversation engine. The first release scope excludes QQ Space, Bilibili, image generation/local arbitrary HTTP integrations and a third-party module marketplace. It also excludes song covers: voice input uses ASR and spoken replies use TTS.

## Current state

This is the first implementation pass. Implemented foundations include OneBot reverse WebSocket intake and HTTP replies; QQ Open Platform Webhook signature verification/validation and Gateway intake; a bounded, ordered conversation queue; OpenAI-compatible chat/ASR/TTS clients; SQLite chat history and memory; private web management panel; persona selection; proactive scheduling; and database backup/restore. QQ Open Platform permissions and supported scenes remain controlled by Tencent. OneBot voice sending requires a local ffmpeg and a configured SILK encoder. QQ official voice sending requires TTS in Opus format. Production credentials and real QQ sandboxes have not been configured here.

The existing plugin is MIT licensed. This rewrite keeps a compatible project license and attributes the original product concept to its project, while keeping its implementation independent.

## Build

Install Rust (stable), a C/C++ linker for your platform, and `ffmpeg` if you need OneBot voice output.

```powershell
cargo test
cargo build --release
```

Create a configuration and check it:

```powershell
.\target\release\aurabot.exe init
```

Edit `config.toml`. Set the model API key as an environment variable. Start in dry-run mode; it records the would-be response and sends nothing:

```powershell
$env:AURABOT_MODEL_KEY = "your-model-api-key"
.\target\release\aurabot.exe check
.\target\release\aurabot.exe serve
```

Open `http://127.0.0.1:8787`. On first start a random management token is written to `data/admin.token`; enter it in the panel. The server listens on loopback by default. To receive QQ Open Platform Webhook events from the internet, set `server.listen` to a public interface and put TLS and authentication for remote panel access at a reverse proxy. Keep the management API behind the proxy's authentication policy too. Alternatively use the official WebSocket transport, which only requires outbound network access.

## Connect OneBot v11

Configure your OneBot implementation's reverse WebSocket to connect to `onebot.ws_url`, enable the adapter and set the bot's own account ID. Set `onebot.http_url` to the same gateway's HTTP API root. Optional bearer tokens are read from the configured environment variable. Verify the received bot ID and begin with `chat.dry_run = true`.

OneBot group and private chats, images, stickers and incoming audio records are parsed. Speech transcription uses the configured OpenAI-compatible ASR endpoint. To send a OneBot voice message, install `ffmpeg` and set `voice.silk_encoder` to a program that reads mono 24 kHz WAV on stdin and writes SILK to stdout. The audio is held in bounded memory and is not retained after sending.

## Connect the QQ Open Platform

Create an official QQ bot and configure `official.app_id`, `official.secret_env`, and either:

- `official.transport = "webhook"`: set the platform callback URL to `https://your-domain/official/webhook`, forward that path to Aurabot, and configure the server's public bind address. The adapter handles URL verification and verifies Ed25519 signatures before queueing events.
- `official.transport = "websocket"`: Aurabot opens the official Gateway connection and reconnects after network failures.

Put the App Secret in the named environment variable. Configure the platform IP allowlist and only subscribe to scenes/events the bot has actually been granted. The same API key environment rules apply to chat, ASR and TTS. Official voice replies currently require `voice.tts_format = "opus"`; media uploads are only implemented for group and C2C private replies. Unsupported scenes fall back to text.

Official QQ account IDs are opaque platform identifiers. They are kept separate from OneBot QQ numbers, and memory/session keys include channel and scene to prevent collisions.

## Run safely

- Keep `chat.enabled = false` and `chat.dry_run = true` until the model, channel, permissions and reply style are checked in the panel.
- Set `chat.owners` to stable platform user IDs before enabling owner commands. OneBot uses QQ IDs. The official bot uses the platform's openid; it is not a QQ number.
- Group replies default to explicit mention. Memory and history are scoped to a channel and conversation. The shared `/remember` memory is inserted only in owner sessions.
- Management requests use a bearer token. Store it in a secret manager as `AURABOT_ADMIN_TOKEN`; otherwise the generated token is stored in `data/admin.token`.
- The HTTP/model queues, context length, response size, media size, history retention and memory store are bounded. SQLite uses WAL and a periodic cleanup command is available in the System page.
- Run `cargo test` before release. End-to-end tests with a live QQ sandbox are still required for each official scene and media path.

## Planned capability set

The target includes chat and interruption controls, independent personalities and mood, voice messages in both directions, emoji collection/use, long-term memory and person profiles, optional desktop pet, proactive private conversation, fine-grained permissions, and a full management panel. QQ Space, Bilibili, image generation and its arbitrary local endpoints, and the module marketplace are out of scope. This first pass establishes the service, account adapters, data store and basic panel; the remaining feature work must be verified against the legacy behavior before calling the rewrite complete.

## License

MIT. See [LICENSE](LICENSE).
