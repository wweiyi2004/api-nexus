# API Nexus

API Nexus is a local API gateway for routing AI model requests through OpenAI-compatible and Anthropic-compatible interfaces.

## Features

- OpenAI-compatible entrypoint: `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/models`
- Anthropic-compatible entrypoint: `/v1/messages`, `/v1/messages/count_tokens`
- Bidirectional protocol conversion between OpenAI chat completions and Anthropic messages
- Per-model provider priority routing with drag-and-drop ordering and fallback
- Provider presets for common model vendors
- Multiple named client API keys with per-key request attribution
- Persistent SQLite request logs with retention controls and CSV export
- Responsive line-chart usage trends and compact expandable request logs
- Provider-specific model pricing with verified preset prices, cache pricing, and USD/CNY cost estimates
- Fusion panel/judge/final routing with optional local web search and page fetching
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

## Fusion Web Tools

Fusion panel and judge models can use `web_search` and `web_fetch` through a local [open-webSearch](https://www.npmjs.com/package/open-websearch) daemon. Start the daemon separately:

```bash
npx open-websearch serve
```

In the Fusion page, enable web tools and enter the loopback daemon URL printed by that command (for example, `http://127.0.0.1:3210`). API Nexus accepts only `localhost`, `127.0.0.1`, or `::1` daemon hosts. When web tools are disabled or no daemon URL is configured, Fusion uses its original single-request model calls.

Fusion proxy routing supports two modes. `forced` always runs panel, judge, and final stages. `on_demand` sends the request to the configured outer model with a server-side `fusion` tool; the outer model can answer directly or invoke panel/judge analysis and then write the final response. Clients can force that invocation with OpenAI `tool_choice: "required"` or Anthropic `tool_choice: {"type":"any"}`. Streaming remains unsupported for both modes.

## Release Automation

GitHub Actions verifies frontend tests, Playwright smoke tests, Rust tests, formatting, and Clippy on every change. Tags matching `v*` trigger the release workflow, which builds installers, emits signed updater artifacts, uploads `latest.json`, and publishes the GitHub Release.

Updater signing secrets are already consumed as `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Authenticode signing additionally requires:

- `WINDOWS_CERTIFICATE`: Base64-encoded trusted code-signing PFX
- `WINDOWS_CERTIFICATE_PASSWORD`: PFX password

When these secrets are absent, the release workflow still publishes Windows installers without Authenticode signing. Tauri updater artifacts remain signed with the updater key.

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
