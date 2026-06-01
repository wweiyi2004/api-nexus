# API Nexus

API Nexus is a local API gateway for routing AI model requests through OpenAI-compatible and Anthropic-compatible interfaces.

## Features

- OpenAI-compatible entrypoint: `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/models`
- Anthropic-compatible entrypoint: `/v1/messages`, `/v1/messages/count_tokens`
- Bidirectional protocol conversion between OpenAI chat completions and Anthropic messages
- Provider priority routing and fallback
- Provider presets for common model vendors
- Local proxy API key protection
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
npm run build
```

Backend:

```bash
cd src-tauri
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

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
