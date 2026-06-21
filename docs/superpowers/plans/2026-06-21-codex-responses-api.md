# Codex Responses API Compatibility Plan

**Goal:** Allow Codex CLI to use `nexus/fusion` as a normal custom model provider with `wire_api = "responses"`, including multi-turn client tool execution.

**Verified client baseline:** Codex CLI `0.141.0` sends `POST /v1/responses` with `stream:true`, `instructions`, a Responses `input` item array, `tools`, `tool_choice`, `parallel_tool_calls`, `reasoning`, `include`, `prompt_cache_key`, and `client_metadata`. Its tool list includes top-level `function` tools, free-form `custom` tools, namespace tools, and hosted tools. Follow-up turns send `function_call_output`/`custom_tool_call_output` items keyed by `call_id`.

**Compatibility boundary:** API Nexus will support text messages, function tools, custom tools, and namespace-contained function/custom tools. Unsupported hosted tools (for example OpenAI-hosted web search) are not advertised to inner OpenAI/Anthropic-compatible models. Codex's local shell, patch, browser, and plugin tools remain client-executed.

## Architecture

- Add `responses.rs` as the protocol adapter. It owns Responses request parsing, conversion to protocol-neutral messages/tool specs, Responses output items, and SSE serialization.
- Add `POST /v1/responses` to the proxy router. Authentication, logging, token accounting, Fusion model detection, and error envelopes remain in `proxy.rs`.
- Generalize the agentic loop to distinguish server tools from client tools:
  - `fusion`, `web_search`, and `web_fetch` can be executed inside API Nexus.
  - Codex tools are returned as Responses output items and executed by Codex.
- Preserve both Fusion modes:
  - `forced`: panel + judge always run; the configured final model receives Codex tools and may return text or a client tool call.
  - `on_demand`: the outer model receives the server-side `fusion` tool plus Codex tools; only the `fusion` call is consumed internally.
- Streaming is initially produced as a valid Responses SSE stream from the completed internal turn. This is not token-by-token upstream streaming, but it satisfies Codex's protocol and keeps the existing bounded Fusion orchestration.

## Task 1: Protocol types and request conversion

- Create `src-tauri/src/responses.rs` and register it in `main.rs`.
- Parse `instructions` and `input` items into OpenAI-style messages.
- Preserve assistant function calls and client `function_call_output` items across turns.
- Convert top-level functions and namespace functions to model-facing `ToolSpec`s with a reversible name mapping.
- Adapt custom tools to a single-string function schema and restore them as `custom_tool_call` output items.
- Add pure unit tests using a reduced fixture based on the captured Codex 0.141.0 request.

## Task 2: Mixed server/client tool loop

- Add an agentic outcome enum: final text or client tool calls, both carrying accumulated usage.
- Execute only explicitly registered server tools.
- Return client tool calls without invoking their executor.
- Reject a response that mixes server and client tool calls in one parallel batch; request `parallel_tool_calls:false` upstream.
- Keep existing Fusion web-tool tests unchanged.

## Task 3: Fusion Responses execution

- Add Responses entry points for forced and on-demand modes.
- Forced mode runs panel/judge and then lets the final model answer or call a Codex tool.
- On-demand mode merges the `fusion` server tool with Codex tools and lets the outer model decide.
- Aggregate outer/final and panel/judge usage.
- Ensure inner panel/judge requests never receive the server-side `fusion` tool.

## Task 4: `/v1/responses` HTTP and SSE

- Add authenticated `POST /v1/responses` routing for `nexus/fusion`.
- Non-streaming response shape: `object:"response"`, completed status, message/function/custom output items, and usage.
- Streaming event minimum accepted by Codex:
  - `response.created`
  - optional `response.output_text.delta`
  - `response.output_item.done` for every final message/tool item
  - `response.completed` with response id and usage
- End every SSE event with a blank line so Codex's eventsource parser emits `response.completed` before connection close.
- Use Responses-style nested errors for invalid requests and upstream failures.

## Task 5: Verification and documentation

- Unit tests for conversion, output items, and SSE framing.
- Proxy integration tests for text, function call, custom tool call, follow-up tool output, forced mode, and on-demand mode.
- Full Rust tests, clippy, rustfmt, frontend tests/build, and Playwright.
- Add the exact Codex `config.toml` and environment-variable setup to README.
- Run a real `codex exec` against API Nexus with a mock upstream model, verify one local tool round-trip, and record the supported Codex version.

## Non-goals for this iteration

- Native upstream Responses API passthrough for ordinary non-Fusion models.
- Stateful `previous_response_id` storage; Codex sends the accumulated item history.
- OpenAI-hosted tools that require OpenAI server execution.
- WebSocket Responses transport or remote compaction endpoints.

