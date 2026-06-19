# API Nexus

API Nexus is a local API gateway for routing AI model requests through OpenAI-compatible and Anthropic-compatible interfaces.

## Features

- OpenAI-compatible entrypoint: `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/models`
- Anthropic-compatible entrypoint: `/v1/messages`, `/v1/messages/count_tokens`
- Bidirectional protocol conversion between OpenAI chat completions and Anthropic messages
- Provider priority routing and fallback
- Provider presets for common model vendors
- Multiple named client API keys with per-key request attribution
- Persistent SQLite request logs with retention controls and CSV export
- Token usage trends with separate cache-read/cache-write pricing and USD/CNY cost estimates
- Windows DPAPI protection for upstream and client API keys
- Signed in-app updates from GitHub Releases
- Local proxy API key protection and background system-tray operation
- Tauri desktop UI with responsive provider and model management pages

## Local Development

```bash
npm install
npm run dev
```

Run the Tauri app:

```bash
npm run tauri dev
```

Build the app:

```bash
npm run tauri build
```

## Verification

Frontend:

```bash
npm test
npm run test:e2e
npm run build
```

Backend:

```bash
cd src-tauri
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Local Data

API Nexus stores local state under `%APPDATA%\api-nexus`:

- `config.json`: non-secret settings only
- `secrets.dpapi`: API keys encrypted for the current Windows user
- `api-nexus.sqlite3`: persistent request logs and usage history

The retention period and maximum number of log entries are configurable in Settings. Logs can be exported as CSV from the Request Log page.

## Release Automation

GitHub Actions verifies frontend tests, Playwright smoke tests, Rust tests, formatting, and Clippy on every change. Tags matching `v*` trigger the release workflow, which builds installers, emits signed updater artifacts, uploads `latest.json`, and publishes the GitHub Release.

Updater signing secrets are already consumed as `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Authenticode signing additionally requires:

- `WINDOWS_CERTIFICATE`: Base64-encoded trusted code-signing PFX
- `WINDOWS_CERTIFICATE_PASSWORD`: PFX password

The release workflow deliberately refuses to publish unsigned Windows installers.

## Client Configuration

OpenAI-compatible clients:

```text
base_url = http://127.0.0.1:11434/v1
```

Anthropic-compatible clients, including Claude Code:

```text
ANTHROPIC_BASE_URL=http://127.0.0.1:11434
```

Use the proxy API key configured in API Nexus as the client API key.
