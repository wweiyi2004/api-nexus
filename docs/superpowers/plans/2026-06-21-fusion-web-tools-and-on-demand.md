# Fusion Web Tools + On-Demand Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring API Nexus Fusion closer to OpenRouter's Fusion Router by (a) giving panel/judge models `web_search` + `web_fetch` via a dual-protocol tool-calling loop backed by a local open-webSearch daemon, and (b) adding an OpenRouter-style on-demand mode where an outer model decides whether to invoke fusion and writes the final answer.

**Architecture:** Introduce a protocol-agnostic agentic tool-calling loop (`agentic.rs`) that both panel/judge calls (Plan 1) and the outer model (Plan 2) drive. Tools are pluggable via a `ToolExecutor` trait. The web tools (`web_tools.rs`) are a thin HTTP client over a locally-run **open-webSearch** daemon — search and fetch are delegated to `POST /search` and `POST /fetch-web`, so api-nexus writes no scraping/HTML-extraction/SSRF code. Fusion's existing model-name routing (`is_fusion_model`) stays as the "always force" path; on-demand mode adds a parallel outer-model path that injects a `fusion` server-tool.

**Tech Stack:** Rust (axum, reqwest, serde_json, tokio), React frontend. **No new Rust crates** — web search/fetch are delegated over HTTP to a user-run open-webSearch daemon (Node, started via `npx open-websearch serve`). No API keys.

## Global Constraints

- Tauri v2 / Rust edition 2021; no new panics in business code (use `Result`), matching existing `proxy.rs`/`fusion.rs` style.
- `cargo test` and `cargo clippy --all-targets` must stay green after **every** task (project history shows clippy is enforced).
- No commit may carry `Co-Authored-By` or any AI-generated attribution (user global rule).
- Behavior of existing Fusion (panel→judge→final, model-name forced trigger, streaming/tool-calling rejection) must remain unchanged where not explicitly modified.
- Streaming stays **rejected** for Fusion in all phases (per decision).
- Web search/fetch are delegated to the local open-webSearch daemon (`POST /search`, `POST /fetch-web`); api-nexus only ever calls the **trusted loopback daemon URL** it is configured with. SSRF defense is the daemon's responsibility; api-nexus must never proxy arbitrary remote callers to the daemon.
- The daemon is **optional**: with no daemon URL configured (or `enable_web_tools=false`), Fusion behaves byte-identically to today (no tools injected).

## Decisions (locked)

| Decision | Choice |
|---|---|
| Search + fetch backend | **open-webSearch** local daemon — `POST /search` + `POST /fetch-web`, configurable URL; no API key, free, includes fetch, multi-engine (incl. Chinese) |
| Tool loop protocols | **Dual** (OpenAI + Anthropic) |
| `web_fetch` | Delegated to daemon `/fetch-web` (no self-written HTML extraction / SSRF) |
| On-demand mode | **OpenRouter-style**: outer model decides + writes final |
| Streaming | **Not supported** (Fusion keeps rejecting `stream:true`) |
| Daemon deployment | Configurable URL; user runs `npx open-websearch serve` (Tauri sidecar auto-spawn deferred to a later iteration) |

### open-webSearch daemon API (verified from docs/http-api.md)

- Binds `127.0.0.1`, default port 3000/3210 (configurable). Response envelope for every endpoint: `{ "status": "ok"|"error", "data": {...}|null, "error": null|{code,message}, "hint": ... }`.
- `GET /health` → liveness.
- `POST /search` body `{ query (required), limit (1-50, default 10), engines? [], searchMode? }` → `data` holds structured results (titles/urls/descriptions). **Exact `data` field names must be confirmed against a running daemon in Task 6** — the doc shows the envelope but not the search `data` shape.
- `POST /fetch-web` body `{ url (required), maxChars? (1000-200000, default 30000) }` → `data` holds the fetched content. **Confirm `data` shape live in Task 5.**

---

## File Structure

**Create:**
- `src-tauri/src/agentic.rs` — protocol-agnostic tool-calling loop + `ToolSpec`/`ToolExecutor`/`ToolCall` types. One responsibility: drive a model through tool calls until it returns text.
- `src-tauri/src/web_tools.rs` — a thin HTTP client over the open-webSearch daemon: `web_search`/`web_fetch` `ToolSpec`s and a `WebTools` struct implementing `ToolExecutor` by POSTing to `{daemon_url}/search` and `{daemon_url}/fetch-web`. No scraping, no SSRF code (delegated).

**Modify:**
- `src-tauri/src/fusion.rs` — `call_model`/`call_model_inner` route through `agentic::run_tool_loop`; `execute_fusion` builds the `WebTools` executor from config and passes it (plus its specs) to panel/judge calls.
- `src-tauri/src/config.rs` — `FusionConfig` gains `web_search_daemon_url: Option<String>`, `enable_web_tools: bool`, `max_tool_calls: u32` (clamped), `web_search_limit: u32`, `web_fetch_max_chars: u32`; defaults + clamps.
- `src-tauri/src/main.rs` — register new modules (`mod agentic; mod web_tools;`).
- `src-tauri/src/proxy.rs` — (Plan 2 only) on-demand path: when an outer model is configured, inject the `fusion` server-tool and drive it through `agentic::run_tool_loop`.
- `src/pages/Fusion.tsx` / `src/pages/Settings.tsx` / `src/types` — frontend config: daemon URL, enable toggle, `max_tool_calls`, (Plan 2) outer model + mode.

No `security.rs` change: the daemon URL is not a secret (no API key).

**Phase → Plan mapping (Scope Check):**
- **Plan 1 (this doc, detailed):** `agentic.rs` loop + `web_tools.rs` daemon client (search **and** fetch) wired into panel/judge. Deliverable: panel/judge can search and fetch the web. Self-contained, testable against a mock daemon.
- **Plan 2 (outline):** On-demand outer-model mode. Builds on Plan 1's loop. Changes existing behavior, kept behind `mode=="on_demand"`.

---

# Plan 1 — Dual-protocol tool loop + open-webSearch tools

## Task 1: Core tool types in `agentic.rs`

**Files:**
- Create: `src-tauri/src/agentic.rs`
- Modify: `src-tauri/src/main.rs` (add `mod agentic;`)
- Test: inline `#[cfg(test)] mod tests` in `agentic.rs`

**Interfaces:**
- Produces:
  - `pub struct ToolSpec { pub name: String, pub description: String, pub input_schema: serde_json::Value }`
  - `pub struct ToolCall { pub id: String, pub name: String, pub arguments: serde_json::Value }`
  - `pub trait ToolExecutor: Send + Sync { async fn execute(&self, call: &ToolCall) -> Result<String, String>; }` via `async_trait`.

- [ ] **Step 1: Add `mod agentic;` to `main.rs`** near the other `mod` lines: `mod agentic;`

- [ ] **Step 2: Write the failing tests for protocol serializers**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_spec_serializes_to_openai_function_tool() {
        let spec = ToolSpec {
            name: "web_fetch".into(),
            description: "Fetch a URL".into(),
            input_schema: json!({"type":"object","properties":{"url":{"type":"string"}}}),
        };
        assert_eq!(spec.to_openai_tool(), json!({
            "type": "function",
            "function": {
                "name": "web_fetch",
                "description": "Fetch a URL",
                "parameters": {"type":"object","properties":{"url":{"type":"string"}}}
            }
        }));
    }

    #[test]
    fn tool_spec_serializes_to_anthropic_tool() {
        let spec = ToolSpec {
            name: "web_fetch".into(),
            description: "Fetch a URL".into(),
            input_schema: json!({"type":"object","properties":{"url":{"type":"string"}}}),
        };
        assert_eq!(spec.to_anthropic_tool(), json!({
            "name": "web_fetch",
            "description": "Fetch a URL",
            "input_schema": {"type":"object","properties":{"url":{"type":"string"}}}
        }));
    }
}
```

- [ ] **Step 3: Run, expect FAIL.** `cargo test --bin api-nexus agentic::tests` → compile error (`ToolSpec` not found).

- [ ] **Step 4: Implement types + serializers**

```rust
use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl ToolSpec {
    pub fn to_openai_tool(&self) -> Value {
        json!({"type":"function","function":{
            "name": self.name, "description": self.description, "parameters": self.input_schema}})
    }
    pub fn to_anthropic_tool(&self) -> Value {
        json!({"name": self.name, "description": self.description, "input_schema": self.input_schema})
    }
}

#[derive(Clone, Debug)]
pub struct ToolCall { pub id: String, pub name: String, pub arguments: Value }

#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> Result<String, String>;
}
```
Add `async-trait = "0.1"` to `src-tauri/Cargo.toml` `[dependencies]`.

- [ ] **Step 5: Run, expect PASS (2 tests).**
- [ ] **Step 6: clippy + commit** `git commit -m "Add tool-spec types and protocol serializers for fusion tool loop"`

---

## Task 2: Parse tool calls from each protocol's response

**Files:** Modify `src-tauri/src/agentic.rs`; inline tests.

**Interfaces:**
- Produces:
  - `pub fn parse_openai_tool_calls(response: &Value) -> (Option<String>, Vec<ToolCall>)` — reads `choices[0].message.{content,tool_calls}`.
  - `pub fn parse_anthropic_tool_calls(response: &Value) -> (Option<String>, Vec<ToolCall>)` — reads `content[]` (`text` / `tool_use{id,name,input}`).

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn parses_openai_tool_calls() {
    let resp = json!({"choices":[{"message":{"content":null,"tool_calls":[
        {"id":"call_1","type":"function","function":{"name":"web_fetch","arguments":"{\"url\":\"https://e.com\"}"}}]}}]});
    let (text, calls) = parse_openai_tool_calls(&resp);
    assert!(text.is_none());
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].name, "web_fetch");
    assert_eq!(calls[0].arguments, json!({"url":"https://e.com"}));
}

#[test]
fn parses_anthropic_tool_use() {
    let resp = json!({"content":[
        {"type":"text","text":"Let me check."},
        {"type":"tool_use","id":"tu_1","name":"web_fetch","input":{"url":"https://e.com"}}]});
    let (text, calls) = parse_anthropic_tool_calls(&resp);
    assert_eq!(text.as_deref(), Some("Let me check."));
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "tu_1");
    assert_eq!(calls[0].arguments, json!({"url":"https://e.com"}));
}
```

- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement parsers**

```rust
pub fn parse_openai_tool_calls(response: &Value) -> (Option<String>, Vec<ToolCall>) {
    let message = response.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("message"));
    let text = message.and_then(|m| m.get("content")).and_then(Value::as_str)
        .filter(|s| !s.is_empty()).map(str::to_string);
    let calls = message.and_then(|m| m.get("tool_calls")).and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|tc| {
            let function = tc.get("function")?;
            let arguments = function.get("arguments").and_then(Value::as_str).unwrap_or("{}");
            Some(ToolCall {
                id: tc.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                name: function.get("name").and_then(Value::as_str)?.to_string(),
                arguments: serde_json::from_str(arguments).unwrap_or_else(|_| json!({})),
            })
        }).collect()).unwrap_or_default();
    (text, calls)
}

pub fn parse_anthropic_tool_calls(response: &Value) -> (Option<String>, Vec<ToolCall>) {
    let mut text = String::new();
    let mut calls = Vec::new();
    if let Some(blocks) = response.get("content").and_then(Value::as_array) {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => { if let Some(t) = block.get("text").and_then(Value::as_str) { text.push_str(t); } }
                Some("tool_use") => calls.push(ToolCall {
                    id: block.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                    name: block.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                    arguments: block.get("input").cloned().unwrap_or_else(|| json!({})),
                }),
                _ => {}
            }
        }
    }
    ((!text.is_empty()).then_some(text), calls)
}
```

- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: clippy + commit** `git commit -m "Parse tool calls from OpenAI and Anthropic responses"`

---

## Task 3: Build follow-up request bodies (inject tools + append tool results)

**Files:** Modify `src-tauri/src/agentic.rs`; inline tests.

**Interfaces:**
- Produces:
  - `pub fn openai_request_body(model: &str, messages: &[Value], max_tokens: u64, tools: &[ToolSpec]) -> Value`
  - `pub fn anthropic_request_body(model: &str, system: Option<&str>, messages: &[Value], max_tokens: u64, tools: &[ToolSpec]) -> Value`
  - `pub fn openai_followup_messages(assistant: &Value, results: &[(ToolCall, String)]) -> Vec<Value>` — the raw assistant message (carrying `tool_calls`) followed by one `{role:"tool",tool_call_id,content}` per result.
  - `pub fn anthropic_followup_messages(results: &[(ToolCall, String)]) -> Vec<Value>` — `[{role:"assistant",content:[tool_use blocks]}, {role:"user",content:[{type:"tool_result",tool_use_id,content}]}]`.

Tests assert each shape (mirror `proxy.rs:925-942` for tool defs and `fusion.rs:628-633` for the base body). Same TDD cycle → commit `"Build tool-injected requests and tool-result messages"`.

> **Implementer note:** `tool_use_id` (Anthropic) vs `tool_call_id` (OpenAI) is the easy mistake. Anthropic requires the prior assistant `tool_use` block echoed back before the `tool_result`.

---

## Task 4: The dual-protocol loop `run_tool_loop`

**Files:** Modify `src-tauri/src/agentic.rs`; inline tests with a mock `ToolExecutor` + a mock model server (reuse the `axum` + `tokio::spawn` pattern from `proxy.rs`/`fusion.rs` tests).

**Interfaces:**
- Consumes: `ToolSpec`, `ToolCall`, `ToolExecutor`, parsers (Task 2), body builders (Task 3), `proxy::is_anthropic_provider`, `proxy::{openai_upstream_url, anthropic_upstream_url}`, `proxy::extract_token_usage`, `proxy::TokenUsage`.
- Produces:
```rust
pub async fn run_tool_loop(
    client: &reqwest::Client,
    provider: &crate::config::Provider,
    model: &str,
    system: Option<&str>,
    messages: Vec<Value>,
    max_tokens: u64,
    tools: &[ToolSpec],
    executor: &dyn ToolExecutor,
    max_tool_calls: u32,
) -> Result<(String, crate::proxy::TokenUsage), String>;
```

**Loop logic (both protocols):**
1. Build request body (inject `tools` only if non-empty), POST to provider, parse response.
2. Accumulate `TokenUsage` each round (`extract_token_usage`).
3. No tool calls → return accumulated text (error if empty, mirroring `non_empty_model_content`).
4. Tool calls but `rounds >= max_tool_calls` → return best text, else error `"exceeded max_tool_calls"`.
5. Else execute each call via `executor.execute`, append assistant + tool-result messages, loop.

- [ ] **Step 1: Failing test — no tool call → returns text + usage** (mock server returns a plain completion).
- [ ] **Step 2: Failing test — one tool call then text** (server returns tool_call on POST #1, text on #2; mock executor records the call; assert executor got parsed args and final text returned).
- [ ] **Step 3: Failing test — `max_tool_calls=0` / empty tools → executor never invoked, first completion returned.**
- [ ] **Step 4: Run, expect FAIL.**
- [ ] **Step 5: Implement `run_tool_loop`** (branch on `is_anthropic_provider`; OpenAI uses `Authorization: Bearer`, Anthropic uses `x-api-key` + `anthropic-version`, like `fusion.rs:635-657`).
- [ ] **Step 6: Run, expect PASS (3 tests).**
- [ ] **Step 7: clippy + commit** `git commit -m "Add dual-protocol agentic tool-calling loop"`

---

## Task 5: open-webSearch daemon client + `web_fetch` tool

**Files:**
- Create: `src-tauri/src/web_tools.rs`
- Modify: `src-tauri/src/main.rs` (`mod web_tools;`)
- Test: inline tests with a mock daemon (axum server returning the `{status,data,error}` envelope).

**Interfaces:**
- Produces:
  - `pub struct WebTools { client: reqwest::Client, daemon_url: String, search_limit: u32, fetch_max_chars: u32 }`
  - `pub fn web_fetch_spec() -> ToolSpec` (input `{url: string}`)
  - `pub fn web_search_spec() -> ToolSpec` (input `{query: string}`)
  - `impl WebTools { pub fn specs(&self) -> Vec<ToolSpec>; async fn call_daemon(&self, path: &str, body: Value) -> Result<Value, String> }`
  - `#[async_trait] impl ToolExecutor for WebTools` — dispatch `web_search` → `/search`, `web_fetch` → `/fetch-web`.

**Daemon envelope handling:** `call_daemon` POSTs JSON, parses `{status,data,error}`; on `status=="error"` returns `Err(error.message)`; otherwise returns `data`. Then a per-tool formatter turns `data` into a compact text string for the model.

- [ ] **Step 1: Failing test — envelope success/error parsing** (`call_daemon` returns `data` on ok, `Err(message)` on error). Mock daemon returns canned envelopes.
- [ ] **Step 2: Failing test — `web_fetch` posts `{url,maxChars}` to `/fetch-web` and returns the formatted content text.** Mock daemon asserts the request body and returns a fetch envelope.
- [ ] **Step 3: Run, expect FAIL.**
- [ ] **Step 4: Implement `WebTools`, specs, `call_daemon`, and the `web_fetch` dispatch + formatter.**

> **Implementer note:** before finalizing the `web_fetch` formatter, run a real daemon (`npx open-websearch serve`) and `curl -X POST .../fetch-web -d '{"url":"https://example.com"}'` to confirm the `data` field names (doc shows `{url, content}` for `/fetch-github-readme`; `/fetch-web`'s `data` shape must be read off the live response). Format to text accordingly.

- [ ] **Step 5: Run, expect PASS.**
- [ ] **Step 6: clippy + commit** `git commit -m "Add open-webSearch daemon client and web_fetch tool"`

---

## Task 6: `web_search` dispatch + config + wire into `fusion::execute_fusion`

**Files:**
- Modify: `src-tauri/src/web_tools.rs` (add `web_search` dispatch + formatter), `src-tauri/src/config.rs` (`FusionConfig` fields + defaults/clamp), `src-tauri/src/fusion.rs` (`call_model` → `run_tool_loop`).
- Test: `web_tools.rs` unit test for search formatting; extend `fusion.rs` integration tests.

**Interfaces:**
- Consumes: `agentic::{run_tool_loop, ToolExecutor}`, `web_tools::WebTools`.
- `FusionConfig` gains: `web_search_daemon_url: Option<String>`, `enable_web_tools: bool` (default false), `max_tool_calls: u32` (default 8, clamp 1–16), `web_search_limit: u32` (default 5, clamp 1–50), `web_fetch_max_chars: u32` (default 30000, clamp 1000–200000).

**Key `fusion.rs` change:** `execute_fusion` builds `Option<WebTools>` from config (Some only when `enable_web_tools && daemon_url set`). `call_model` calls `agentic::run_tool_loop` with the executor's `specs()` + `max_tool_calls` when present, else **empty tools + `max_tool_calls=0`** so the loop returns the first completion — byte-identical to today.

- [ ] **Step 1: Failing test — `web_search` formats daemon `data` results into text** (canned envelope; confirm field names live first, see note).
- [ ] **Step 2: Failing test — panel call with web tools enabled drives a `web_search` tool call against a mock daemon and the result reaches recorded panel content.**
- [ ] **Step 3: Failing test — `enable_web_tools=false`: existing `openai_fusion_model_runs_panel_judge_and_final_steps` passes unchanged (no tools injected).**
- [ ] **Step 4: Run, expect FAIL.**
- [ ] **Step 5: Implement config fields + clamp, `web_search` dispatch/formatter, and the `call_model` rewrite.**
- [ ] **Step 6: Run full suite, expect PASS (`cargo test`); diagnose with `--test-threads=1` if the known passthrough flakiness appears.**
- [ ] **Step 7: clippy + commit** `git commit -m "Wire open-webSearch web_search/web_fetch into fusion panel/judge"`

---

## Task 7: Frontend config (daemon URL + toggle)

**Files:** Modify `src/pages/Fusion.tsx` (or `Settings.tsx`), `src/types`, and the config command/types if `FusionConfig` is surfaced; extend `src/test` config tests.

- [ ] Add `enableWebTools`, `webSearchDaemonUrl`, `maxToolCalls` (and optional `webSearchLimit`, `webFetchMaxChars`) to the fusion config form; persist via the existing config command; update types/tests; `npm run test`; commit `"Add web-tools daemon config to Fusion settings"`.

---

# Plan 2 — On-demand outer-model mode (outline)

Builds on Plan 1's loop. **Changes Fusion's architecture**, so it is isolated and fully behind `mode=="on_demand"`.

- **Config:** `FusionConfig.outer_model: Option<ModelRef>` and `mode: "forced" | "on_demand"` (default `forced` = today's behavior). On-demand requires `outer_model`.
- **Fusion as a server tool:** define `fusion_tool_spec()` (no-arg or `{focus?:string}`). When `mode==on_demand` and the request model is a fusion model, the proxy handler drives the **outer model** through `agentic::run_tool_loop` with `[fusion_tool_spec()]`; the executor's `execute` runs the existing `execute_fusion` (panel→judge) and returns the judge analysis as the tool result; the outer model then writes the final answer.
- **`tool_choice:"required"` passthrough:** forward the client's `tool_choice` to the outer model so callers can force fusion (OpenRouter's "forcing fusion on every request").
- **Recursion guard:** flag inner panel/judge calls so they never inject the fusion tool (mirrors OpenRouter's `x-openrouter-fusion-depth`).
- **Wiring point:** `proxy.rs` `try_fusion` gains a branch — `forced` → current `execute_fusion` path; `on_demand` → outer-model loop. The `Prepared`/`ApiDialect` plumbing from the recent refactor carries the dialect through.
- **Tests:** outer model declines fusion → returns its own answer; outer model calls fusion → final reflects judge analysis; `tool_choice:"required"` forces the call; recursion guard prevents nested fusion.
- **Streaming:** still rejected (per decision) — no SSE work.

---

## Testing Strategy

- **Unit-first (TDD):** serializers/parsers/formatters are pure functions with table tests (no network).
- **Integration via local servers:** reuse `spawn_router` + `tokio::spawn` for the loop, the mock model server, and the **mock open-webSearch daemon** (return the `{status,data,error}` envelope).
- **Regression guard:** existing 6 fusion tests + 8 error-body characterization tests must stay green; `enable_web_tools=false` / `mode=="forced"` paths must be byte-identical to today.
- **Run** with `--test-threads=1` when diagnosing the known pre-existing flakiness in `token_usage_is_recorded_for_passthrough_responses`.
- **clippy** after every task.

## Risks & Notes

- **Anthropic field names** (`tool_use_id`, `stop_reason:"tool_use"`, `input` vs `arguments`, echoing the assistant `tool_use` block) — verify against a live response in Task 4.
- **Daemon `data` shapes** for `/search` and `/fetch-web` are not fully specified in the doc — confirm against a running daemon before finalizing formatters (Tasks 5–6).
- **Cost/loops:** `max_tool_calls` (default 8, clamp 1–16) is the only bound on inner-call fan-out.
- **Deployment:** the daemon is user-run (`npx open-websearch serve`) for now; document it. Auto-spawn via Tauri sidecar is a later iteration (needs Node-runtime packaging).
- **Daemon availability:** if the daemon is down, tool calls fail; `WebTools` errors should degrade gracefully (the panel model still gets the error as a tool result and can answer without it) rather than failing the whole fusion run.
- Plan 2 is the only phase that changes existing behavior; keep it fully behind `mode=="on_demand"`.
