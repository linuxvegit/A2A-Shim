# A2A-Shim Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a Rust binary `a2a-shim` with two subcommands — `serve` (wraps a spawned ACP Agent process and exposes it over Google A2A HTTP) and `client` (a stdio MCP server exposing an `a2a_send` tool that calls remote A2A endpoints) — enabling multi-agent HTTP discussions where a Host (e.g. Claude Code) consults specialist ACP Agents.

**Architecture:** Single binary, Cargo workspace with four library/binary members plus a non-shipped mock test harness:

- `a2a-shim` — CLI binary, dispatches subcommands.
- `a2a-shim-core` — shared wire types, error codes, timeouts, config, logging init.
- `a2a-shim-serve` — A2A HTTP server, per-`conversation_id` ACP session reuse, SSE bridge.
- `a2a-shim-client` — stdio MCP server, `a2a_send` tool, outbound A2A JSON-RPC + SSE consumption.
- `tests/mock_acp_agent` — scripted stdio mock used by Serve integration tests.

**Tech stack:** Rust 1.75+, `tokio` 1, `axum` 0.7, `reqwest` 0.12 (rustls), `serde`/`serde_json`, `agent-client-protocol = "0.13"`, `tracing` + `tracing-subscriber`, `clap` 4, `miette` 7, `eventsource-stream` 0.2, `uuid` 1, `toml` 0.8.

**Authoritative references — the spec is the source of truth, not this plan:**

- Spec: [`docs/superpowers/specs/2026-06-03-a2a-shim-design.md`](../specs/2026-06-03-a2a-shim-design.md). Every wire shape, state-machine rule, error code, CLI flag, and TOML key is defined there with byte-exact JSON examples. **Each task below cites the relevant section(s); the implementer MUST read those sections before writing code and MUST treat the spec as authoritative if this plan and the spec ever drift.**
- Glossary: [`CONTEXT.md`](../../../CONTEXT.md) — nine canonical terms (`conversation_id`, `Task`, `session/update`, etc.).
- ADRs:
  - [`0001`](../../adr/0001-serve-shim-acp-client-capabilities.md) — Serve Shim sets `clientCapabilities.fs = false` and `terminal = false`.
  - [`0002`](../../adr/0002-serve-shim-mcp-servers-empty.md) — Serve Shim sends `mcpServers = []` on `session/new`.
  - [`0003`](../../adr/0003-client-shim-progress-heartbeat.md) — Client Shim emits `notifications/progress` every 30 s under a long-running `a2a_send` call.
  - [`0004`](../../adr/0004-conversation-id-flat-namespace.md) — `conversation_id` is an opaque flat string; clients SHOULD use `"<host>/<topic>"`.

**Working agreements applied to every task in this plan:**

1. **TDD.** Write the failing test first, watch it fail, write the minimum to pass, commit. The "watch it fail" step is what proves the test actually exercises the new code.
2. **DRY / YAGNI.** No code unless an executable test or an explicit spec requirement demands it. Defer v1.1+ ideas to the spec's TODO anchors; do not invent new ones.
3. **No placeholders in shipped code.** No `unimplemented!()`, no `todo!()`, no "fill in later" comments. Mock binaries under `tests/` are *not* shipped and may scaffold behavior incrementally.
4. **Small files.** Most files stay under 200 lines; split before committing if they grow past that.
5. **Commit per task, not per step.** A task is the atomic unit of progress. If a task can't be completed in one bite-sized session, the plan is wrong — escalate before continuing.
6. **Run only the tests this task added or modified** unless the step says otherwise. The final integration task in Phase 4 runs `cargo test --workspace`.
7. **Tracing.** Every new top-level async function in `a2a-shim-serve` and `a2a-shim-client` MUST be `#[tracing::instrument]` with a `skip` list that excludes large payloads but includes correlation ids (`conversation_id`, `task_id`).

---

## File Structure Map

```
a2a-shim/
├── Cargo.toml                              # workspace root (Task 2)
├── rust-toolchain.toml                     # Task 2
├── .gitignore                              # already present; extended in Task 2
├── crates/
│   ├── a2a-shim/                           # main binary
│   │   ├── Cargo.toml
│   │   ├── src/{main,cli}.rs
│   │   └── tests/cli_help.rs
│   ├── a2a-shim-core/
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── constants.rs                # Task 3
│   │   │   ├── logging.rs                  # Task 14
│   │   │   ├── wire/{mod,envelope,message,methods,task,sse,card}.rs   # Tasks 5–10
│   │   │   ├── error/{mod,codes,normalize}.rs                          # Task 11
│   │   │   ├── timeout/{mod,idle,ceiling}.rs                           # Task 12
│   │   │   └── config/{mod,serve_toml}.rs                              # Task 13
│   │   └── tests/                          # one file per module above
│   ├── a2a-shim-serve/
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── run.rs                      # entry, wiring (Task 27)
│   │   │   ├── acp_client.rs               # spawn + initialize ACP Agent (Task 17)
│   │   │   ├── conversation.rs             # ConversationMap + H1 guard (Task 16)
│   │   │   ├── task_registry.rs            # TaskBinding state machine (Task 18)
│   │   │   ├── bridge.rs                   # session/update -> SSE translation (Task 19)
│   │   │   ├── permission.rs               # §2.7 strategies (Task 20)
│   │   │   ├── elicitation.rs              # §2.8 method-not-implemented (Task 21)
│   │   │   ├── agent_card.rs               # /.well-known/agent.json (Task 23)
│   │   │   ├── http.rs                     # axum router + JSON-RPC dispatch (Tasks 24-26)
│   │   │   ├── sse_sink.rs                 # broadcast + keepalive (Task 15)
│   │   │   ├── idle_sweep.rs               # background idle reaper (Task 27)
│   │   │   └── shutdown.rs                 # signal handling (Task 27)
│   │   └── tests/                          # serve_* suites
│   └── a2a-shim-client/
│       ├── Cargo.toml
│       ├── src/
│       │   ├── lib.rs
│       │   ├── run.rs                      # entry, wiring (Task 33)
│       │   ├── mcp_server.rs               # stdio JSON-RPC loop (Task 29)
│       │   ├── tool_schema.rs              # a2a_send tool definition (Task 28)
│       │   ├── outbound.rs                 # reqwest + eventsource-stream (Task 30)
│       │   ├── heartbeat.rs                # 30 s notifications/progress (Task 31)
│       │   ├── render.rs                   # tool-result serialization (Task 32)
│       │   └── cancellation.rs             # §3.9 (Task 32)
│       └── tests/                          # client_* suites
├── tests/
│   └── mock_acp_agent/                     # scripted stdio mock (Task 22)
│       ├── Cargo.toml
│       └── src/main.rs
└── verify/                                 # Phase 0 throwaway, NOT in workspace
    ├── Cargo.toml
    ├── src/main.rs
    └── REPORT.md
```

---

## Phase 0 — Dependency Reality Check (Spec §6.6)

Verifies load-bearing assumptions before any production code. **If V1, V3, or V8 fail, stop and revisit the relevant ADR or spec section with the user.** V2 may fall back from `agent-client-protocol::mcp_server` to a separate MCP crate. V4 and V5 may be marked `DEFERRED` if Claude Code does not yet send `_meta.progressToken` on `tools/call`.

### Task 1: Reality-check spike

**Files:** create `verify/Cargo.toml`, `verify/src/main.rs`, `verify/.gitignore`, `verify/REPORT.md`.

Note: `verify/` lives at repo root but is **deliberately not** in the workspace `members` list (Task 2). It is throwaway code with relaxed quality bars.

- [ ] **Step 1 — Install reference ACP Agent**

```bash
npm install -g @agentclientprotocol/claude-agent-acp
claude-agent-acp --version
```

Record the version. If `npm` is missing, install Node.js LTS first.

- [ ] **Step 2 — Scaffold `verify/Cargo.toml`**

```toml
[package]
name = "verify"
version = "0.0.1"
edition = "2021"
publish = false

[dependencies]
agent-client-protocol = "0.13"
tokio = { version = "1", features = ["full"] }
serde_json = "1"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

`verify/.gitignore`:

```
target/
run.log
```

- [ ] **Step 3 — Write the probe (`verify/src/main.rs`)**

Before writing code, fetch `https://docs.rs/agent-client-protocol/0.13` to learn the **current** 0.13.x API surface — the crate is in active development and method signatures may have shifted since the spec was authored. Then implement a `tokio::main` that:

1. Initializes `tracing_subscriber::fmt::init()`.
2. Spawns `claude-agent-acp` via `tokio::process::Command` with piped stdio.
3. Sends `initialize` with `clientCapabilities = { fs: { readTextFile: false, writeTextFile: false }, terminal: false }` (ADR 0001).
4. Sends `session/new` with `mcpServers = []` (ADR 0002) and `cwd = std::env::current_dir()?`.
5. Sends `session/prompt` with text `"What is 2+2? Reply with just the number."`.
6. Prints every `session/update` notification and the final `stopReason`.
7. Sends `session/cancel`, then sends a **second** `session/prompt` on the **same** session id, prints whether it succeeds (this answers V8).
8. Returns `Ok(())`.

Clarity over elegance — this code is thrown away.

- [ ] **Step 4 — Run the probe**

```bash
cd verify
export ANTHROPIC_API_KEY=...
cargo run 2>&1 | tee run.log
cd ..
```

- [ ] **Step 5 — Author `verify/REPORT.md`** using this template:

```markdown
# Phase 0 Reality Check — A2A-Shim
Date: <today>
agent-client-protocol version: 0.13.<patch>
claude-agent-acp version: <step 1>

## V1 — Basic ACP flow works against claude-agent-acp
[ ] PASS / [ ] FAIL — Evidence: <log snippet>

## V2 — Usable MCP server crate for Client Shim stdio loop
Investigated: agent-client-protocol::mcp_server, rmcp, mcp-server, hand-rolled.
Decision: <which crate> — Reasons: <…>

## V3 — clientCapabilities.fs=false, terminal=false works (ADR 0001)
[ ] PASS / [ ] FAIL — Evidence: <…>
If FAIL: ADR 0001 must be revised; document refusal behavior.

## V4 — Claude Code sends _meta.progressToken on tools/call (ADR 0003)
[ ] PASS / [ ] FAIL / [ ] DEFERRED — Method: <…>

## V5 — Claude Code renders notifications/progress as "alive" (ADR 0003)
[ ] PASS / [ ] FAIL / [ ] DEFERRED — Evidence: <…>

## V6 — session/request_permission frequency under claude-agent-acp
Observations: <…>  Implication for permission strategy default: <…>

## V7 — elicitation/create currently emitted by claude-agent-acp
[ ] YES / [ ] NO — Evidence: <…>

## V8 — ACP Agent accepts new session/prompt after session/cancel
[ ] PASS / [ ] FAIL — Evidence: <step 3.7>

## Action items
- For each FAIL: link to ADR or spec section that must be amended before Phase 1.
- Otherwise: proceed to Phase 1.
```

- [ ] **Step 6 — Commit**

```bash
git add verify/Cargo.toml verify/src verify/REPORT.md verify/.gitignore
git commit -m "chore(phase-0): reality-check verification crate and report"
```

- [ ] **Step 7 — Decision gate.** If V1, V3, or V8 are FAIL, stop and re-spec. Otherwise continue.

---

## Phase 1 — Workspace + Core Wire Layer

End state: `cargo build --workspace` succeeds; every wire type, error code, timeout helper, and config loader has a passing round-trip or behavior test. Zero behavior in `serve`/`client` crates yet.

### Task 2: Cargo workspace scaffold

**Files:** create `Cargo.toml`, `rust-toolchain.toml`; modify `.gitignore`.

- [ ] **Step 1 — Workspace root `Cargo.toml`** with all shared deps centralized:

```toml
[workspace]
resolver = "2"
members = [
    "crates/a2a-shim",
    "crates/a2a-shim-core",
    "crates/a2a-shim-client",
    "crates/a2a-shim-serve",
    "tests/mock_acp_agent",
]

[workspace.package]
version      = "0.1.0"
edition      = "2021"
rust-version = "1.75"
license      = "Apache-2.0"

[workspace.dependencies]
tokio                = { version = "1",    features = ["full"] }
tokio-util           = { version = "0.7",  features = ["io"] }
serde                = { version = "1",    features = ["derive"] }
serde_json           = "1"
anyhow               = "1"
thiserror            = "1"
tracing              = "0.1"
tracing-subscriber   = { version = "0.3",  features = ["json", "env-filter"] }
clap                 = { version = "4",    features = ["derive", "env"] }
uuid                 = { version = "1",    features = ["v4", "serde"] }
miette               = { version = "7",    features = ["fancy"] }
toml                 = "0.8"
axum                 = { version = "0.7",  features = ["json"] }
reqwest              = { version = "0.12", default-features = false, features = ["rustls-tls", "stream", "json"] }
eventsource-stream   = "0.2"
futures              = "0.3"
bytes                = "1"
agent-client-protocol = "0.13"

[profile.release]
opt-level     = 3
lto           = "thin"
codegen-units = 1
strip         = true
panic         = "abort"
```

- [ ] **Step 2 — `rust-toolchain.toml`**:

```toml
[toolchain]
channel    = "1.75"
components = ["rustfmt", "clippy"]
profile    = "minimal"
```

- [ ] **Step 3 — Extend `.gitignore`**:

```
target/
verify/target/
verify/run.log
*.tmp
.DS_Store
```

- [ ] **Step 4 — Commit**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore
git commit -m "feat(scaffold): initialize Cargo workspace with pinned shared deps"
```

### Task 3: Member crate stubs

**Files:** under `crates/{a2a-shim,a2a-shim-core,a2a-shim-serve,a2a-shim-client}/` create `Cargo.toml` + minimal `src/lib.rs` or `src/main.rs`; create `tests/mock_acp_agent/{Cargo.toml,src/main.rs}` as a stub.

Each member's `Cargo.toml` inherits from `[workspace.package]` and uses `dep.workspace = true` form. Only depend on what each crate actually needs — e.g. `a2a-shim-serve` pulls `axum` and `agent-client-protocol`, `a2a-shim-client` pulls `reqwest` and `eventsource-stream`, both pull `a2a-shim-core` by path. The binary crate `a2a-shim` declares `[[bin]] name = "a2a-shim"` and depends on all three library crates plus `clap`, `tokio`, `anyhow`, `tracing`, `miette`.

`a2a-shim-core/src/lib.rs`:

```rust
//! Shared logic for the A2A-Shim project.
pub mod constants;
```

`a2a-shim-core/src/constants.rs`:

```rust
//! Project-wide constants pinned by the design spec.
use std::time::Duration;

/// A2A `Message.metadata` key carrying `conversation_id` (spec §2.6, §4.4).
pub const CONVERSATION_METADATA_KEY: &str = "x-a2a-shim/conversation";

/// SSE keepalive cadence emitted by the Serve Shim (spec §2.12).
pub const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Client Shim `notifications/progress` heartbeat cadence (ADR 0003).
pub const MCP_PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Wire-protocol revision independent of crate version.
pub const PROTOCOL_VERSION: &str = "0.1";
```

`a2a-shim-serve/src/lib.rs`, `a2a-shim-client/src/lib.rs`: a single doc-comment line each.

`a2a-shim/src/main.rs` (placeholder, replaced in Task 4):

```rust
fn main() { println!("a2a-shim placeholder; CLI wired in Task 4"); }
```

`tests/mock_acp_agent/src/main.rs` (stubbed; filled in Task 22):

```rust
fn main() { eprintln!("mock-acp-agent: stubbed; behavior added in Task 22"); }
```

- [ ] **Step 1 — Author each `Cargo.toml`** as described.
- [ ] **Step 2 — Author each stub source file** as described.
- [ ] **Step 3 — Build** `cargo build --workspace`. Unused-import warnings are acceptable on stubs; compilation errors are not.
- [ ] **Step 4 — Commit** `feat(scaffold): create workspace members with placeholder entries`.

### Task 4: CLI subcommand dispatch

**Files:** create `crates/a2a-shim/src/cli.rs`; replace `crates/a2a-shim/src/main.rs`; create `crates/a2a-shim/tests/cli_help.rs`.

Spec reference: §5.1–5.3 (full CLI surface, env-var fallbacks, log-format options).

- [ ] **Step 1 — Failing test** — `tests/cli_help.rs`:

```rust
use std::process::Command;
fn bin() -> &'static str { env!("CARGO_BIN_EXE_a2a-shim") }

#[test]
fn help_lists_both_subcommands() {
    let out = Command::new(bin()).arg("--help").output().expect("run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("serve"),  "help missing serve: {stdout}");
    assert!(stdout.contains("client"), "help missing client: {stdout}");
}

#[test]
fn serve_help_shows_listen_flag() {
    let out = Command::new(bin()).args(["serve", "--help"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--listen"), "serve --help missing --listen: {stdout}");
}

#[test]
fn client_help_shows_heartbeat_flag() {
    let out = Command::new(bin()).args(["client", "--help"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--heartbeat-secs"), "client --help missing --heartbeat-secs: {stdout}");
}
```

Run: `cargo test -p a2a-shim --test cli_help` — expect FAIL.

- [ ] **Step 2 — Implement `crates/a2a-shim/src/cli.rs`**:

```rust
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "a2a-shim", version,
          about = "Bidirectional shim between ACP agents and Google A2A protocol")]
pub struct Cli {
    /// Increase log verbosity (-v info, -vv debug, -vvv trace).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
    #[arg(short, long, global = true)] pub quiet: bool,
    /// Log format: compact | json | pretty.
    #[arg(long, global = true, default_value = "compact")] pub log_format: String,
    #[command(subcommand)] pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Spawn an ACP Agent and serve A2A over HTTP.
    Serve(ServeOpts),
    /// Run as stdio MCP server exposing `a2a_send`.
    Client(ClientOpts),
}

#[derive(Args, Debug)]
pub struct ServeOpts {
    #[arg(short, long, env = "A2A_SHIM_CONFIG")] pub config: Option<PathBuf>,
    #[arg(long)] pub listen: Option<String>,
    #[arg(long)] pub advertised_endpoint: Option<String>,
    /// Override `[agent].command` (whitespace-split into command+args).
    #[arg(long)] pub spawn: Option<String>,
    #[arg(long)] pub cwd: Option<PathBuf>,
    #[arg(long, value_parser = ["auto_approve", "auto_reject"])]
    pub permission_strategy: Option<String>,
    #[arg(long)] pub log_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ClientOpts {
    #[arg(long, default_value_t = 120, env = "A2A_SHIM_CONNECT_TIMEOUT_SECS")] pub connect_timeout_secs: u64,
    #[arg(long, default_value_t = 600, env = "A2A_SHIM_STREAM_IDLE_SECS")]     pub stream_idle_secs: u64,
    #[arg(long, default_value_t = 86400, env = "A2A_SHIM_HARD_CEILING_SECS")]  pub hard_ceiling_secs: u64,
    /// MCP notifications/progress heartbeat cadence (ADR 0003).
    #[arg(long, default_value_t = 30, env = "A2A_SHIM_HEARTBEAT_SECS")]        pub heartbeat_secs: u64,
    /// Log file path. Logs NEVER go to stdout — stdout is the MCP transport.
    #[arg(long, env = "A2A_SHIM_LOG_FILE")] pub log_file: Option<PathBuf>,
    #[arg(long, default_value = "info", env = "A2A_SHIM_LOG_LEVEL")] pub log_level: String,
}
```

- [ ] **Step 3 — Replace `crates/a2a-shim/src/main.rs`**:

```rust
mod cli;
use clap::Parser;

fn main() -> anyhow::Result<()> {
    let parsed = cli::Cli::parse();
    match parsed.command {
        cli::Command::Serve(_)  => { eprintln!("a2a-shim serve: implemented in Task 27");  std::process::exit(2); }
        cli::Command::Client(_) => { eprintln!("a2a-shim client: implemented in Task 33"); std::process::exit(2); }
    }
}
```

- [ ] **Step 4 — PASS**: `cargo test -p a2a-shim --test cli_help`.
- [ ] **Step 5 — Commit** `feat(cli): clap subcommand dispatch for serve/client`.

### Task 5: JSON-RPC envelope types (spec §4.3)

**Files:** create `crates/a2a-shim-core/src/wire/{mod,envelope}.rs`; create `tests/envelope_roundtrip.rs`; add `pub mod wire;` to `lib.rs`.

- [ ] **Step 1 — Failing test** (`tests/envelope_roundtrip.rs`) covers: request preserves `id` and `method` through round-trip; success response serializes with the literal `"result"` key; error response serializes with `"error"` and round-trips with code `-32001`.

```rust
use a2a_shim_core::wire::envelope::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, ResultOrError};
use serde_json::{json, Value};

#[test]
fn request_roundtrip() {
    let req: JsonRpcRequest<Value> = JsonRpcRequest {
        jsonrpc: "2.0", id: json!("req-1"),
        method: "message/send".into(), params: json!({"foo": 42}),
    };
    let back: JsonRpcRequest<Value> = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
    assert_eq!(back.id, json!("req-1"));
    assert_eq!(back.method, "message/send");
}

#[test]
fn response_result_serializes_with_result_key() {
    let resp: JsonRpcResponse<Value> = JsonRpcResponse {
        jsonrpc: "2.0", id: json!(7),
        result_or_error: ResultOrError::from_result(json!({"ok": true})),
    };
    let s = serde_json::to_string(&resp).unwrap();
    assert!(s.contains(r#""result":{"ok":true}"#), "got: {s}");
}

#[test]
fn response_error_roundtrips() {
    let resp: JsonRpcResponse<Value> = JsonRpcResponse {
        jsonrpc: "2.0", id: json!(7),
        result_or_error: ResultOrError::from_error(JsonRpcError { code: -32001, message: "Task not found".into(), data: None }),
    };
    let back: JsonRpcResponse<Value> = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
    match back.result_or_error {
        ResultOrError::Error { error } => assert_eq!(error.code, -32001),
        _ => panic!("expected error"),
    }
}
```

- [ ] **Step 2 — Implement** `src/wire/mod.rs` (`pub mod envelope;`) and `src/wire/envelope.rs`:

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest<P> {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub method: String,
    pub params: P,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse<R> {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(flatten)] pub result_or_error: ResultOrError<R>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResultOrError<R> {
    Result { result: R },
    Error  { error: JsonRpcError },
}

impl<R> ResultOrError<R> {
    pub fn from_result(r: R) -> Self { Self::Result { result: r } }
    pub fn from_error(e: JsonRpcError) -> Self { Self::Error { error: e } }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub data: Option<Value>,
}
```

Add `pub mod wire;` to `lib.rs`. PASS. Commit `feat(wire): JSON-RPC envelope types with round-trip tests`.

### Task 6: A2A `Message` / `Part` / `MessageMetadata` (spec §4.4, §2.6)

**Files:** `src/wire/message.rs`, `tests/message_roundtrip.rs`; add `pub mod message;` to `wire/mod.rs`.

- [ ] **Step 1 — Failing test** covers: text-part round-trip; metadata extracts `x-a2a-shim/conversation` AND preserves arbitrary unknown keys via `#[serde(flatten)]`; file-part with `bytes`; data-part with arbitrary `Value`; default `MessageMetadata` omits the conversation key when serialized.

```rust
use a2a_shim_core::wire::message::{Message, MessageMetadata, MessageRole, Part};
use serde_json::json;

#[test]
fn text_message_roundtrip() {
    let m = Message { role: MessageRole::User,
        parts: vec![Part::Text { text: "hello".into() }], metadata: None };
    let back: Message = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(back.role, MessageRole::User);
}

#[test]
fn metadata_extracts_conversation_and_preserves_unknown_keys() {
    let raw = r#"{"role":"user","parts":[{"type":"text","text":"hi"}],
                  "metadata":{"x-a2a-shim/conversation":"alice/review","x-custom/marker":"keep"}}"#;
    let m: Message = serde_json::from_str(raw).unwrap();
    let md = m.metadata.unwrap();
    assert_eq!(md.conversation.as_deref(), Some("alice/review"));
    assert_eq!(md.extra.get("x-custom/marker"), Some(&json!("keep")));
    assert!(serde_json::to_string(&m).unwrap().contains("x-custom/marker"));
}

#[test]
fn file_part_with_bytes() {
    let raw = r#"{"type":"file","name":"x.png","mimeType":"image/png","bytes":"AAAA"}"#;
    let Part::File { name, mime_type, bytes, uri } = serde_json::from_str(raw).unwrap()
        else { panic!("expected File") };
    assert_eq!(name.as_deref(), Some("x.png"));
    assert_eq!(mime_type.as_deref(), Some("image/png"));
    assert_eq!(bytes.as_deref(), Some("AAAA"));
    assert!(uri.is_none());
}

#[test]
fn data_part_carries_arbitrary_value() {
    let Part::Data { data } = serde_json::from_str(r#"{"type":"data","data":{"x":1}}"#).unwrap()
        else { panic!() };
    assert_eq!(data, json!({"x":1}));
}

#[test]
fn default_metadata_omits_conversation_key() {
    let s = serde_json::to_string(&MessageMetadata::default()).unwrap();
    assert!(!s.contains("x-a2a-shim/conversation"));
}
```

- [ ] **Step 2 — Implement** `src/wire/message.rs`:

```rust
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole { User, Agent }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Part {
    Text { text: String },
    File {
        #[serde(skip_serializing_if = "Option::is_none")] name: Option<String>,
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")] mime_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")] bytes: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")] uri:   Option<String>,
    },
    Data { data: Value },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageMetadata {
    #[serde(rename = "x-a2a-shim/conversation", default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    #[serde(flatten)] pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")] pub metadata: Option<MessageMetadata>,
}
```

Add `pub mod message;`. PASS. Commit `feat(wire): Message/Part/MessageMetadata with passthrough fields`.

### Task 7: A2A `Task` / `TaskStatus` / `TaskState` / `Artifact` / `TaskId` (spec §2.5, §4.4)

**Files:** `src/wire/task.rs`, `tests/task_roundtrip.rs`; add `pub mod task;`.

- [ ] **Step 1 — Failing test** asserts: kebab-case serialization for every state; `is_terminal()` true for `Completed`/`Failed`/`Canceled`, false for the others; full `Task` round-trip with `contextId`, `history`, `artifacts`; `TaskId::new_random()` returns `"t-<simple uuid>"`.

```rust
use a2a_shim_core::wire::message::{Message, MessageRole, Part};
use a2a_shim_core::wire::task::{Artifact, Task, TaskId, TaskState, TaskStatus};

#[test]
fn state_serializes_kebab_case() {
    use TaskState::*;
    for (s, lit) in [(Submitted,"\"submitted\""),(Working,"\"working\""),
                     (InputRequired,"\"input-required\""),(Completed,"\"completed\""),
                     (Failed,"\"failed\""),(Canceled,"\"canceled\"")] {
        assert_eq!(serde_json::to_string(&s).unwrap(), lit);
    }
}

#[test]
fn terminal_classifier_matches_spec_2_5() {
    use TaskState::*;
    for s in [Submitted, Working, InputRequired] { assert!(!s.is_terminal(), "{s:?}"); }
    for s in [Completed, Failed, Canceled]       { assert!(s.is_terminal(),  "{s:?}"); }
}

#[test]
fn task_full_roundtrip() {
    let task = Task {
        id: TaskId::from("t-abc"), context_id: Some("alice/review".into()),
        status: TaskStatus { state: TaskState::Completed, message: None, timestamp: Some("2026-06-03T10:00:00Z".into()) },
        history: vec![Message { role: MessageRole::User, parts: vec![Part::Text { text: "hi".into() }], metadata: None }],
        artifacts: vec![Artifact { artifact_id: Some("a-1".into()), name: Some("answer".into()),
            parts: vec![Part::Text { text: "ok".into() }], metadata: None }],
        metadata: None,
    };
    let back: Task = serde_json::from_str(&serde_json::to_string(&task).unwrap()).unwrap();
    assert_eq!(back.id.as_str(), "t-abc");
    assert_eq!(back.context_id.as_deref(), Some("alice/review"));
    assert!(back.status.state.is_terminal());
}

#[test]
fn task_id_random_shape() {
    let id = TaskId::new_random();
    assert!(id.as_str().starts_with("t-") && id.as_str().len() > 5);
}
```

- [ ] **Step 2 — Implement** `src/wire/task.rs`:

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;
use super::message::{Message, Part};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(pub String);
impl TaskId {
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn new_random() -> Self { Self(format!("t-{}", uuid::Uuid::new_v4().simple())) }
}
impl From<&str>   for TaskId { fn from(s: &str)   -> Self { Self(s.to_owned()) } }
impl From<String> for TaskId { fn from(s: String) -> Self { Self(s) } }
impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState { Submitted, Working, InputRequired, Completed, Failed, Canceled }
impl TaskState {
    pub fn is_terminal(self) -> bool { matches!(self, Self::Completed | Self::Failed | Self::Canceled) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatus {
    pub state: TaskState,
    #[serde(skip_serializing_if = "Option::is_none")] pub message:   Option<Message>,
    #[serde(skip_serializing_if = "Option::is_none")] pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(rename = "artifactId", skip_serializing_if = "Option::is_none")] pub artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]                         pub name:        Option<String>,
    pub parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")]                         pub metadata:    Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    #[serde(rename = "contextId", skip_serializing_if = "Option::is_none")] pub context_id: Option<String>,
    pub status: TaskStatus,
    #[serde(default)]                                                       pub history:    Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]                pub artifacts:  Vec<Artifact>,
    #[serde(skip_serializing_if = "Option::is_none")]                       pub metadata:   Option<Value>,
}
```

Add `pub mod task;`. PASS. Commit `feat(wire): Task/TaskStatus/TaskState/Artifact + TaskId helpers`.

### Task 8: Method-param types `SendMessageParams`, `TaskIdParams` (spec §4.4)

**Files:** `src/wire/methods.rs`, `tests/methods_roundtrip.rs`; add `pub mod methods;`.

- [ ] **Step 1 — Failing test**: new-Task path omits `id` from JSON; continuation path includes `"id":"t-1"`; `TaskIdParams` serializes to exactly `{"id":"t-zzz"}`.

```rust
use a2a_shim_core::wire::message::{Message, MessageRole, Part};
use a2a_shim_core::wire::methods::{SendMessageParams, TaskIdParams};
use a2a_shim_core::wire::task::TaskId;

#[test]
fn new_task_omits_id() {
    let p = SendMessageParams { id: None,
        message: Message { role: MessageRole::User, parts: vec![Part::Text { text: "hi".into() }], metadata: None },
        configuration: None };
    assert!(!serde_json::to_string(&p).unwrap().contains("\"id\""));
}

#[test]
fn continuation_includes_id() {
    let p = SendMessageParams { id: Some(TaskId::from("t-1")),
        message: Message { role: MessageRole::User, parts: vec![Part::Text { text: "x".into() }], metadata: None },
        configuration: None };
    assert!(serde_json::to_string(&p).unwrap().contains(r#""id":"t-1""#));
}

#[test]
fn task_id_params_is_lean() {
    assert_eq!(serde_json::to_string(&TaskIdParams { id: TaskId::from("t-zzz") }).unwrap(),
               r#"{"id":"t-zzz"}"#);
}
```

- [ ] **Step 2 — Implement** `src/wire/methods.rs`:

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;
use super::message::Message;
use super::task::TaskId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageParams {
    /// Absent => new Task. Present => continuation (only legal in `input-required`, spec §2.5).
    #[serde(skip_serializing_if = "Option::is_none")] pub id: Option<TaskId>,
    pub message: Message,
    /// Accepted on the wire but ignored in MVP.
    #[serde(skip_serializing_if = "Option::is_none")] pub configuration: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskIdParams { pub id: TaskId }
```

PASS. Commit `feat(wire): SendMessageParams + TaskIdParams`.

### Task 9: SSE event codec (spec §4.5)

**Files:** `src/wire/sse.rs`, `tests/sse_codec.rs`; add `pub mod sse;`.

- [ ] **Step 1 — Failing test**: encoded `StatusUpdate` starts with `data: `, ends with `\n\n`, contains `"kind":"status-update"` and `"final":true`; parse of a `status-update` JSON line yields the right variant; parse of an `artifact-update` JSON line yields the `ArtifactUpdate` variant.

```rust
use a2a_shim_core::wire::sse::{encode_sse_event, parse_sse_data_line, SseEvent};
use a2a_shim_core::wire::task::{TaskId, TaskState, TaskStatus};

#[test]
fn encode_status_update_has_final_flag() {
    let ev = SseEvent::StatusUpdate {
        task_id: TaskId::from("t-x"),
        status: TaskStatus { state: TaskState::Completed, message: None, timestamp: None },
        final_: true,
    };
    let s = encode_sse_event(&ev);
    assert!(s.starts_with("data: ") && s.ends_with("\n\n"));
    assert!(s.contains("\"final\":true") && s.contains("\"kind\":\"status-update\""));
}

#[test]
fn parse_status_update() {
    let line = r#"{"kind":"status-update","taskId":"t-x","status":{"state":"working"},"final":false}"#;
    let SseEvent::StatusUpdate { task_id, status, final_ } = parse_sse_data_line(line).unwrap()
        else { panic!() };
    assert_eq!(task_id.as_str(), "t-x");
    assert_eq!(status.state, TaskState::Working);
    assert!(!final_);
}

#[test]
fn parse_artifact_update() {
    let line = r#"{"kind":"artifact-update","taskId":"t-x","artifact":{"parts":[{"type":"text","text":"hi"}]},"append":false}"#;
    assert!(matches!(parse_sse_data_line(line).unwrap(), SseEvent::ArtifactUpdate { .. }));
}
```

- [ ] **Step 2 — Implement** `src/wire/sse.rs`:

```rust
use serde::{Deserialize, Serialize};
use super::task::{Artifact, TaskId, TaskStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SseEvent {
    StatusUpdate {
        #[serde(rename = "taskId")] task_id: TaskId,
        status: TaskStatus,
        #[serde(default, rename = "final")] final_: bool,
    },
    ArtifactUpdate {
        #[serde(rename = "taskId")] task_id: TaskId,
        artifact: Artifact,
        #[serde(default)] append: bool,
    },
}

/// Encode one event as a single SSE `data:` record terminated by a blank line.
/// Keepalive `: keepalive\n\n` frames are emitted separately by the SSE sink.
pub fn encode_sse_event(ev: &SseEvent) -> String {
    let json = serde_json::to_string(ev).expect("SseEvent serialization is infallible");
    format!("data: {json}\n\n")
}

pub fn parse_sse_data_line(line: &str) -> Result<SseEvent, serde_json::Error> {
    serde_json::from_str(line)
}
```

PASS. Commit `feat(wire): SSE event codec (status-update + artifact-update)`.

### Task 10: AgentCard types (spec §2.10, §4.7)

**Files:** `src/wire/card.rs`, `tests/card_roundtrip.rs`; add `pub mod card;`.

- [ ] **Step 1 — Failing test** asserts the canonical JSON shape from spec §2.10: `capabilities.streaming=true`, `capabilities.pushNotifications=false`, `capabilities.stateTransitionHistory=true`, `metadata["x-a2a-shim/conversations"].supported=true`, `.metadataKey="x-a2a-shim/conversation"`, `.contextIdAlias=true`.

```rust
use a2a_shim_core::wire::card::{
    AgentCapabilities, AgentCard, AgentCardMetadata, ConversationsCapability,
};

#[test]
fn card_canonical_shape_matches_spec_2_10() {
    let card = AgentCard {
        name: "claude-code-sidecar".into(),
        description: "Claude Agent exposed as an A2A endpoint".into(),
        version: "0.1.0".into(),
        url: "http://127.0.0.1:7001/".into(),
        capabilities: AgentCapabilities { streaming: true, push_notifications: false, state_transition_history: true },
        default_input_modes:  vec!["text/plain".into()],
        default_output_modes: vec!["text/plain".into()],
        skills: vec![],
        metadata: Some(AgentCardMetadata { conversations: Some(ConversationsCapability {
            supported: true, metadata_key: "x-a2a-shim/conversation".into(),
            context_id_alias: true, max_active: 64, idle_secs: 86400,
        }) }),
    };
    let v = serde_json::to_value(&card).unwrap();
    assert_eq!(v["capabilities"]["streaming"], true);
    assert_eq!(v["capabilities"]["pushNotifications"], false);
    assert_eq!(v["capabilities"]["stateTransitionHistory"], true);
    let conv = &v["metadata"]["x-a2a-shim/conversations"];
    assert_eq!(conv["supported"], true);
    assert_eq!(conv["metadataKey"], "x-a2a-shim/conversation");
    assert_eq!(conv["contextIdAlias"], true);
}
```

- [ ] **Step 2 — Implement** `src/wire/card.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String, pub description: String, pub version: String, pub url: String,
    pub capabilities: AgentCapabilities,
    #[serde(rename = "defaultInputModes")]  pub default_input_modes:  Vec<String>,
    #[serde(rename = "defaultOutputModes")] pub default_output_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub skills: Vec<AgentSkill>,
    #[serde(skip_serializing_if = "Option::is_none")] pub metadata: Option<AgentCardMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    pub streaming: bool,
    #[serde(rename = "pushNotifications")]      pub push_notifications: bool,
    #[serde(rename = "stateTransitionHistory")] pub state_transition_history: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill { pub id: String, pub name: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCardMetadata {
    #[serde(rename = "x-a2a-shim/conversations", skip_serializing_if = "Option::is_none")]
    pub conversations: Option<ConversationsCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationsCapability {
    pub supported: bool,
    #[serde(rename = "metadataKey")]    pub metadata_key: String,
    #[serde(rename = "contextIdAlias")] pub context_id_alias: bool,
    #[serde(rename = "maxActive")]      pub max_active: u32,
    #[serde(rename = "idleSecs")]       pub idle_secs: u64,
}
```

PASS. Commit `feat(wire): AgentCard with x-a2a-shim/conversations extension`.

### Task 11: Error codes + `NormalizedError` envelope (spec §4.6, §3.8)

**Files:** `src/error/{mod,codes,normalize}.rs`, `tests/error_normalize.rs`; add `pub mod error;` to `lib.rs`.

- [ ] **Step 1 — Failing test** asserts every code matches spec §4.6, and that `NormalizedErrorEnvelope` serializes as `{"error":{...}}` with `kind` rendered in `snake_case`, and round-trips every `ErrorKind` variant.

```rust
use a2a_shim_core::error::codes;
use a2a_shim_core::error::normalize::{ErrorKind, NormalizedError, NormalizedErrorEnvelope};

#[test]
fn codes_match_spec_4_6() {
    assert_eq!(codes::PARSE_ERROR,                -32700);
    assert_eq!(codes::INVALID_REQUEST,            -32600);
    assert_eq!(codes::METHOD_NOT_FOUND,           -32601);
    assert_eq!(codes::INVALID_PARAMS,             -32602);
    assert_eq!(codes::INTERNAL_ERROR,             -32603);
    assert_eq!(codes::TASK_NOT_FOUND,             -32001);
    assert_eq!(codes::TASK_NOT_CANCELABLE,        -32002);
    assert_eq!(codes::CONVERSATION_BUSY,          -32010);
    assert_eq!(codes::CONVERSATION_LIMIT_REACHED, -32011);
}

#[test]
fn envelope_wraps_under_error_key() {
    let env = NormalizedErrorEnvelope(NormalizedError {
        kind: ErrorKind::RemoteTimeout, message: "Remote did not respond within 120 seconds".into(),
        remote_task_id: None,
    });
    let v = serde_json::to_value(&env).unwrap();
    assert_eq!(v["error"]["kind"], "remote_timeout");
    assert!(v["error"]["message"].as_str().unwrap().contains("did not respond"));
    assert!(v["error"]["remote_task_id"].is_null());
}

#[test]
fn all_kinds_roundtrip() {
    for kind in [
        ErrorKind::NetworkError, ErrorKind::RemoteTimeout, ErrorKind::RemoteFailed,
        ErrorKind::RemoteCanceled, ErrorKind::ProtocolError, ErrorKind::InvalidRequest,
        ErrorKind::ConcurrentCallNotSupported,
    ] {
        let env = NormalizedErrorEnvelope(NormalizedError { kind, message: "x".into(), remote_task_id: None });
        let back: NormalizedErrorEnvelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(back.0.kind, kind);
    }
}
```

- [ ] **Step 2 — Implement**:

`src/error/mod.rs`: `pub mod codes; pub mod normalize;`

`src/error/codes.rs`:

```rust
//! JSON-RPC + A2A-Shim error codes (spec §4.6).
pub const PARSE_ERROR:                i32 = -32700;
pub const INVALID_REQUEST:            i32 = -32600;
pub const METHOD_NOT_FOUND:           i32 = -32601;
pub const INVALID_PARAMS:             i32 = -32602;
pub const INTERNAL_ERROR:             i32 = -32603;
pub const TASK_NOT_FOUND:             i32 = -32001;
pub const TASK_NOT_CANCELABLE:        i32 = -32002;
pub const CONVERSATION_BUSY:          i32 = -32010;
pub const CONVERSATION_LIMIT_REACHED: i32 = -32011;
```

`src/error/normalize.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    NetworkError, RemoteTimeout, RemoteFailed, RemoteCanceled,
    ProtocolError, InvalidRequest, ConcurrentCallNotSupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedError {
    pub kind: ErrorKind,
    pub message: String,
    pub remote_task_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NormalizedErrorEnvelope(pub NormalizedError);

impl Serialize for NormalizedErrorEnvelope {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = ser.serialize_map(Some(1))?;
        m.serialize_entry("error", &self.0)?;
        m.end()
    }
}
impl<'de> Deserialize<'de> for NormalizedErrorEnvelope {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)] struct W { error: NormalizedError }
        Ok(NormalizedErrorEnvelope(W::deserialize(de)?.error))
    }
}
```

Add `pub mod error;` to `lib.rs`. PASS. Commit `feat(error): JSON-RPC + shim error codes and NormalizedError envelope`.

### Task 12: Timeouts — `IdleGuard` + `HardCeiling`

**Files:** `src/timeout/{mod,idle,ceiling}.rs`, `tests/timeout.rs`; add `pub mod timeout;`.

- [ ] **Step 1 — Failing test** uses `#[tokio::test(start_paused = true)]` to drive virtual time: `IdleGuard::would_trip_now()` returns `Some` only after the window elapses; `reset()` extends it; `HardCeiling::exceeded()` flips after the limit.

```rust
use a2a_shim_core::timeout::ceiling::HardCeiling;
use a2a_shim_core::timeout::idle::IdleGuard;
use std::time::Duration;

#[tokio::test(start_paused = true)]
async fn idle_fires_after_window() {
    let g = IdleGuard::new(Duration::from_secs(2));
    tokio::time::advance(Duration::from_millis(1900)).await;
    assert!(g.would_trip_now().is_none());
    tokio::time::advance(Duration::from_millis(200)).await;
    assert!(g.would_trip_now().is_some());
}

#[tokio::test(start_paused = true)]
async fn idle_reset_extends_window() {
    let mut g = IdleGuard::new(Duration::from_secs(2));
    tokio::time::advance(Duration::from_millis(1500)).await;
    g.reset();
    tokio::time::advance(Duration::from_millis(1500)).await;
    assert!(g.would_trip_now().is_none());
}

#[tokio::test(start_paused = true)]
async fn ceiling_fires_after_window() {
    let c = HardCeiling::new(Duration::from_secs(5));
    tokio::time::advance(Duration::from_secs(4)).await;
    assert!(!c.exceeded());
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(c.exceeded());
}
```

- [ ] **Step 2 — Implement**:

`src/timeout/mod.rs`: `pub mod ceiling; pub mod idle;`

`src/timeout/idle.rs`:

```rust
use std::time::Duration;
use tokio::time::Instant;

#[derive(Debug)]
pub struct IdleGuard { window: Duration, last_activity: Instant }
impl IdleGuard {
    pub fn new(window: Duration) -> Self { Self { window, last_activity: Instant::now() } }
    pub fn reset(&mut self) { self.last_activity = Instant::now(); }
    pub fn would_trip_now(&self) -> Option<Duration> {
        let e = self.last_activity.elapsed();
        (e >= self.window).then_some(e)
    }
}
```

`src/timeout/ceiling.rs`:

```rust
use std::time::Duration;
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub struct HardCeiling { started: Instant, limit: Duration }
impl HardCeiling {
    pub fn new(limit: Duration) -> Self { Self { started: Instant::now(), limit } }
    pub fn exceeded(&self) -> bool { self.started.elapsed() >= self.limit }
    pub fn remaining(&self) -> Option<Duration> {
        let e = self.started.elapsed();
        (e < self.limit).then(|| self.limit - e)
    }
}
```

Add `pub mod timeout;`. PASS. Commit `feat(timeout): IdleGuard + HardCeiling with paused-time tests`.

### Task 13: Serve TOML config loader (spec §2.3)

**Files:** `src/config/{mod,serve_toml}.rs`, `tests/serve_config.rs`; add `pub mod config;`.

- [ ] **Step 1 — Failing test** covers four cases: a minimal TOML with only `[agent]` populates every default from spec §2.3; full override flips every value; `strategy = "passthrough"` returns an error whose message mentions both `passthrough` and `v1.2`; missing `[agent]` is a parse error.

```rust
use a2a_shim_core::config::serve_toml::{PermissionStrategy, ServeConfig};

#[test]
fn defaults_with_only_required() {
    let cfg = ServeConfig::from_toml_str(r#"
[agent]
command = "claude-agent-acp"
cwd = "/tmp/work"
"#).unwrap();
    assert_eq!(cfg.server.listen, "127.0.0.1:7001");
    assert_eq!(cfg.server.conversations.idle_secs, 86400);
    assert_eq!(cfg.server.conversations.max_active, 64);
    assert_eq!(cfg.agent.permissions.strategy, PermissionStrategy::AutoApprove);
    assert!(cfg.agent.permissions.deny_tool_kinds.is_empty());
    assert_eq!(cfg.timeouts.agent_sync_idle_secs, 120);
    assert_eq!(cfg.timeouts.agent_stream_idle_secs, 600);
}

#[test]
fn full_override_parses() {
    let cfg = ServeConfig::from_toml_str(r#"
[server]
listen = "0.0.0.0:9000"
advertised_endpoint = "https://x.example.com"
[server.conversations]
idle_secs = 3600
max_active = 8
[agent]
command = "/usr/local/bin/codex-acp"
args = ["--flag"]
cwd = "/srv/agent"
[agent.permissions]
strategy = "auto_reject"
deny_tool_kinds = ["delete", "execute"]
[timeouts]
agent_sync_idle_secs = 60
agent_stream_idle_secs = 300
shutdown_grace_secs = 10
"#).unwrap();
    assert_eq!(cfg.server.listen, "0.0.0.0:9000");
    assert_eq!(cfg.server.advertised_endpoint.as_deref(), Some("https://x.example.com"));
    assert_eq!(cfg.server.conversations.max_active, 8);
    assert_eq!(cfg.agent.args, vec!["--flag".to_string()]);
    assert_eq!(cfg.agent.permissions.strategy, PermissionStrategy::AutoReject);
    assert_eq!(cfg.agent.permissions.deny_tool_kinds, vec!["delete", "execute"]);
    assert_eq!(cfg.timeouts.shutdown_grace_secs, 10);
}

#[test]
fn passthrough_rejected_with_v1_2_note() {
    let err = ServeConfig::from_toml_str(r#"
[agent]
command = "x"
cwd = "/x"
[agent.permissions]
strategy = "passthrough"
"#).unwrap_err().to_string();
    assert!(err.contains("passthrough"));
    assert!(err.contains("v1.2"));
}

#[test]
fn missing_agent_is_error() {
    let err = ServeConfig::from_toml_str("[server]\nlisten=\"127.0.0.1:7001\"\n")
        .unwrap_err().to_string().to_lowercase();
    assert!(err.contains("agent"));
}
```

- [ ] **Step 2 — Implement**:

`src/config/mod.rs`: `pub mod serve_toml;`

`src/config/serve_toml.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServeConfigError {
    #[error("TOML parse error: {0}")] Parse(#[from] toml::de::Error),
    #[error("`passthrough` permission strategy is reserved for v1.2 and is not implemented in MVP. Use `auto_approve` or `auto_reject`.")]
    PassthroughNotImplemented,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServeConfig {
    #[serde(default)] pub server:   ServerConfig,
    pub agent: AgentConfig,
    #[serde(default)] pub timeouts: TimeoutsConfig,
    #[serde(default)] pub logging:  LoggingConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "d_listen")]    pub listen: String,
    #[serde(default)]                  pub advertised_endpoint: Option<String>,
    #[serde(default = "d_card_path")]  pub agent_card_path: String,
    #[serde(default)]                  pub conversations: ConversationsConfig,
}
impl Default for ServerConfig {
    fn default() -> Self { Self { listen: d_listen(), advertised_endpoint: None, agent_card_path: d_card_path(), conversations: Default::default() } }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConversationsConfig {
    #[serde(default = "d_idle_secs")]  pub idle_secs:  u64,
    #[serde(default = "d_max_active")] pub max_active: u32,
}
impl Default for ConversationsConfig {
    fn default() -> Self { Self { idle_secs: d_idle_secs(), max_active: d_max_active() } }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    pub command: String,
    #[serde(default)] pub args: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default)] pub env: HashMap<String, String>,
    #[serde(default)] pub card: AgentCardConfig,
    #[serde(default)] pub permissions: PermissionsConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AgentCardConfig {
    #[serde(default = "d_card_name")]    pub name: String,
    #[serde(default = "d_card_desc")]    pub description: String,
    #[serde(default = "d_card_version")] pub version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PermissionsConfig {
    #[serde(default)] pub strategy: PermissionStrategy,
    #[serde(default)] pub deny_tool_kinds: Vec<String>,
}
impl Default for PermissionsConfig {
    fn default() -> Self { Self { strategy: PermissionStrategy::AutoApprove, deny_tool_kinds: vec![] } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionStrategy { AutoApprove, AutoReject, Passthrough }
impl Default for PermissionStrategy { fn default() -> Self { Self::AutoApprove } }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TimeoutsConfig {
    #[serde(default = "d_sync_idle")]   pub agent_sync_idle_secs:    u64,
    #[serde(default = "d_stream_idle")] pub agent_stream_idle_secs:  u64,
    #[serde(default = "d_hard_ceil")]   pub agent_hard_ceiling_secs: u64,
    #[serde(default = "d_input_wait")]  pub input_required_wait_secs: u64,
    #[serde(default = "d_shutdown")]    pub shutdown_grace_secs:     u64,
}
impl Default for TimeoutsConfig {
    fn default() -> Self { Self {
        agent_sync_idle_secs: d_sync_idle(), agent_stream_idle_secs: d_stream_idle(),
        agent_hard_ceiling_secs: d_hard_ceil(), input_required_wait_secs: d_input_wait(),
        shutdown_grace_secs: d_shutdown(),
    } }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoggingConfig {
    #[serde(default = "d_log_level")]  pub level:  String,
    #[serde(default = "d_log_format")] pub format: String,
    #[serde(default)]                   pub file:   Option<PathBuf>,
}
impl Default for LoggingConfig {
    fn default() -> Self { Self { level: d_log_level(), format: d_log_format(), file: None } }
}

fn d_listen()       -> String { "127.0.0.1:7001".into() }
fn d_card_path()    -> String { "/.well-known/agent.json".into() }
fn d_idle_secs()    -> u64 { 86400 }
fn d_max_active()   -> u32 { 64 }
fn d_card_name()    -> String { "a2a-shim-serve".into() }
fn d_card_desc()    -> String { "ACP Agent exposed as A2A endpoint via a2a-shim".into() }
fn d_card_version() -> String { "0.1.0".into() }
fn d_sync_idle()    -> u64 { 120 }
fn d_stream_idle()  -> u64 { 600 }
fn d_hard_ceil()    -> u64 { 86400 }
fn d_input_wait()   -> u64 { 86400 }
fn d_shutdown()     -> u64 { 5 }
fn d_log_level()    -> String { "info".into() }
fn d_log_format()   -> String { "compact".into() }

impl ServeConfig {
    pub fn from_toml_str(s: &str) -> Result<Self, ServeConfigError> {
        let cfg: ServeConfig = toml::from_str(s)?;
        if cfg.agent.permissions.strategy == PermissionStrategy::Passthrough {
            return Err(ServeConfigError::PassthroughNotImplemented);
        }
        Ok(cfg)
    }
}
```

Add `pub mod config;` to `lib.rs`. PASS. Commit `feat(config): Serve TOML loader with passthrough rejection`.

### Task 14: Tracing-subscriber init helper

**Files:** create `src/logging.rs`, `tests/logging_smoke.rs`; add `pub mod logging;` to `lib.rs`.

- [ ] **Step 1 — Failing test**: defaults are Stderr + Compact + `info`; calling `try_init` twice is idempotent and does not panic.

```rust
use a2a_shim_core::logging::{LogDestination, LogFormat, LoggingOptions};

#[test]
fn options_default_to_stderr_compact_info() {
    let o = LoggingOptions::default();
    assert!(matches!(o.destination, LogDestination::Stderr));
    assert!(matches!(o.format,      LogFormat::Compact));
    assert_eq!(o.level, "info");
}

#[test]
fn init_idempotent_no_panic() {
    let _ = a2a_shim_core::logging::try_init(LoggingOptions::default());
    let _ = a2a_shim_core::logging::try_init(LoggingOptions::default());
}
```

- [ ] **Step 2 — Implement** `src/logging.rs`:

```rust
//! Centralized tracing-subscriber init.
//!
//! Client Shim hard rule: logs MUST NEVER go to stdout (stdout is the MCP
//! transport). Use Stderr or File destinations.

use std::path::PathBuf;
use std::sync::OnceLock;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[derive(Debug, Clone)]
pub struct LoggingOptions { pub level: String, pub format: LogFormat, pub destination: LogDestination }
impl Default for LoggingOptions {
    fn default() -> Self { Self { level: "info".into(), format: LogFormat::Compact, destination: LogDestination::Stderr } }
}

#[derive(Debug, Clone, Copy)] pub enum LogFormat { Compact, Json, Pretty }
#[derive(Debug, Clone)]       pub enum LogDestination { Stderr, File(PathBuf) }

static INIT: OnceLock<()> = OnceLock::new();

pub fn try_init(opts: LoggingOptions) -> Result<(), TracingInitError> {
    if INIT.get().is_some() { return Ok(()); }
    let filter = EnvFilter::try_new(&opts.level).map_err(|e| TracingInitError::Filter(e.to_string()))?;
    let writer = match &opts.destination {
        LogDestination::Stderr => BoxedWriter::Stderr,
        LogDestination::File(p) => {
            let f = std::fs::OpenOptions::new().create(true).append(true).open(p)
                .map_err(|e| TracingInitError::OpenFile(p.clone(), e.to_string()))?;
            BoxedWriter::File(std::sync::Arc::new(std::sync::Mutex::new(f)))
        }
    };
    let layer = fmt::layer().with_writer(writer);
    let reg = tracing_subscriber::registry().with(filter);
    let r = match opts.format {
        LogFormat::Compact => reg.with(layer.compact()).try_init(),
        LogFormat::Json    => reg.with(layer.json()).try_init(),
        LogFormat::Pretty  => reg.with(layer.pretty()).try_init(),
    };
    r.map_err(|e| TracingInitError::AlreadyInit(e.to_string()))?;
    let _ = INIT.set(());
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum TracingInitError {
    #[error("invalid log filter: {0}")] Filter(String),
    #[error("failed to open log file {0}: {1}")] OpenFile(PathBuf, String),
    #[error("tracing already initialized: {0}")] AlreadyInit(String),
}

#[derive(Clone)]
enum BoxedWriter { Stderr, File(std::sync::Arc<std::sync::Mutex<std::fs::File>>) }
impl<'a> fmt::MakeWriter<'a> for BoxedWriter {
    type Writer = Box<dyn std::io::Write + Send>;
    fn make_writer(&'a self) -> Self::Writer {
        match self {
            BoxedWriter::Stderr   => Box::new(std::io::stderr()),
            BoxedWriter::File(a)  => Box::new(a.lock().expect("log mutex").try_clone().expect("clone fd")),
        }
    }
}
```

Add `pub mod logging;`. PASS. Commit `feat(logging): tracing-subscriber init with stderr/file destinations`.

**Phase 1 exit gate:** `cargo build --workspace && cargo test --workspace` (Phase 1 tests only). All green. `git log --oneline` shows ~13 commits since `8327fd3`.

---

## Phase 2 — Serve Shim

End state: `a2a-shim serve --config sample.toml` boots, spawns the ACP Agent, exposes `/.well-known/agent.json` plus the JSON-RPC root, routes `message/send` and `message/stream` per `conversation_id`, supports `tasks/cancel`, and is exercised end-to-end against `mock-acp-agent`.

Each Phase 2 task ships under the same TDD discipline: failing test → minimum impl → commit. To keep the plan tight, code blocks below show **structure, public surface, and the assertions the test must make**; the implementer fleshes out internals against the spec section cited in each task header. The spec — not this plan — is authoritative for every wire shape, error code, and JSON example.

### Task 15: SSE sink — broadcast + keepalive (spec §2.12, §4.5)

**Files:** create `crates/a2a-shim-serve/src/sse_sink.rs`, `tests/sse_sink.rs`; add `pub mod sse_sink;` to `lib.rs`.

`SseSink` owns a `tokio::sync::broadcast::Sender<SseFrame>` per Task. Frames are either `Event(SseEvent)` or `Keepalive`. `publish_final` sends the terminal event then drops the sender so subscribers observe channel-closed. `start_keepalive(interval)` spawns a background task that publishes `Keepalive` every `interval` and exits when the sender is gone. Capacity is bounded; on `RecvError::Lagged` the SSE responder (Task 25) drops the subscriber and lets the client reconnect — we do not catch up.

- [ ] **Step 1 — Failing test** asserts: (a) a subscriber receives a `working` event, then the terminal `completed` event, then sees the channel closed; (b) with paused time, `start_keepalive(1s)` produces a `Keepalive` frame just after virtual time advances 1100 ms.

```rust
use a2a_shim_serve::sse_sink::{SseFrame, SseSink};
use a2a_shim_core::wire::sse::SseEvent;
use a2a_shim_core::wire::task::{TaskId, TaskState, TaskStatus};
use std::time::Duration;
use tokio::time::timeout;

fn status(state: TaskState, final_: bool) -> SseEvent {
    SseEvent::StatusUpdate { task_id: TaskId::from("t-1"),
        status: TaskStatus { state, message: None, timestamp: None }, final_ }
}

#[tokio::test]
async fn subscriber_receives_event_then_final_closes_channel() {
    let sink = SseSink::new(8);
    let mut rx = sink.subscribe();
    sink.publish_event(status(TaskState::Working, false));
    sink.publish_final(status(TaskState::Completed, true));
    timeout(Duration::from_secs(1), rx.recv()).await.unwrap().unwrap();
    timeout(Duration::from_secs(1), rx.recv()).await.unwrap().unwrap();
    assert!(timeout(Duration::from_secs(1), rx.recv()).await.unwrap().is_err());
}

#[tokio::test(start_paused = true)]
async fn keepalive_emitted_at_interval() {
    let sink = SseSink::new(8);
    let mut rx = sink.subscribe();
    sink.start_keepalive(Duration::from_secs(1));
    tokio::time::advance(Duration::from_millis(1100)).await;
    assert!(matches!(rx.recv().await.unwrap(), SseFrame::Keepalive));
}
```

- [ ] **Step 2 — Implement** the public surface:

```rust
use a2a_shim_core::wire::sse::SseEvent;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub enum SseFrame { Event(SseEvent), Keepalive }

#[derive(Clone)]
pub struct SseSink {
    tx: Arc<std::sync::Mutex<Option<broadcast::Sender<SseFrame>>>>,
}

impl SseSink {
    pub fn new(capacity: usize) -> Self { /* (tx, _) = broadcast::channel; wrap */ unimplemented!() }
    pub fn subscribe(&self) -> broadcast::Receiver<SseFrame> { unimplemented!() }
    pub fn publish_event(&self, event: SseEvent) { unimplemented!() }
    /// Sends the terminal event, then drops the sender so subscribers observe Closed.
    pub fn publish_final(&self, event: SseEvent) { unimplemented!() }
    /// Spawns a tokio task that publishes Keepalive frames at `interval` until the sender is gone.
    pub fn start_keepalive(&self, interval: Duration) { unimplemented!() }
}
```

> The `unimplemented!()` markers above appear **only in this plan** to keep code samples short. The shipped implementation MUST contain the full body — placeholders in shipped code are prohibited by Working Agreement #3.

PASS. Commit `feat(serve): SseSink with broadcast + keepalive`.

### Task 16: `Conversation` + `ConversationMap` with H1 serial guard (spec §2.6)

**Files:** `src/conversation.rs`, `tests/conversation_map.rs`; add `pub mod conversation;`.

H1 (spec §2.6): a given `conversation_id` may only have one in-flight prompt at a time; overlap returns `CONVERSATION_BUSY` (-32010).

- [ ] **Step 1 — Failing test** covers:

1. first sight creates entry and returns `created = true` with the spawned `acp_session_id`;
2. second sight reuses the same entry without calling the spawn closure;
3. `max_active = 2` rejects the third creation with `LimitReached`;
4. `acquire_in_flight` returns `Busy` while a prior guard exists, succeeds after it is dropped;
5. `sweep_idle` removes entries whose `last_used_at` is older than the window (paused time).

```rust
use a2a_shim_serve::conversation::{AcquireError, ConversationMap, NewError};
use std::time::Duration;

#[tokio::test]
async fn first_sight_creates_returns_session() {
    let map = ConversationMap::new(8, Duration::from_secs(3600));
    let (conv, created) = map.get_or_create("alice/review", || Ok::<_,()>("sess-1".into())).await.unwrap();
    assert!(created); assert_eq!(conv.acp_session_id, "sess-1");
}

#[tokio::test]
async fn second_sight_reuses_without_calling_spawn() {
    let map = ConversationMap::new(8, Duration::from_secs(3600));
    map.get_or_create("c", || Ok::<_,()>("s1".into())).await.unwrap();
    let mut spawned = false;
    let (conv, created) = map.get_or_create("c", || { spawned = true; Ok::<_,()>("s2".into()) }).await.unwrap();
    assert!(!created && !spawned && conv.acp_session_id == "s1");
}

#[tokio::test]
async fn max_active_rejects_third() {
    let map = ConversationMap::new(2, Duration::from_secs(3600));
    map.get_or_create("a", || Ok::<_,()>("s".into())).await.unwrap();
    map.get_or_create("b", || Ok::<_,()>("s".into())).await.unwrap();
    assert!(matches!(map.get_or_create("c", || Ok::<_,()>("s".into())).await.unwrap_err(), NewError::LimitReached));
}

#[tokio::test]
async fn busy_guard_rejects_overlap_then_releases() {
    let map = ConversationMap::new(8, Duration::from_secs(3600));
    map.get_or_create("c", || Ok::<_,()>("s1".into())).await.unwrap();
    let permit = map.acquire_in_flight("c").await.expect("first");
    assert!(matches!(map.acquire_in_flight("c").await, Err(AcquireError::Busy)));
    drop(permit);
    assert!(map.acquire_in_flight("c").await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn idle_sweep_drops_old_entries() {
    let map = ConversationMap::new(8, Duration::from_secs(2));
    map.get_or_create("c", || Ok::<_,()>("s".into())).await.unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(map.sweep_idle().await, vec!["c".to_string()]);
}
```

- [ ] **Step 2 — Implement** the public surface:

```rust
pub type SessionId = String;

pub struct Conversation {
    pub id: String,
    pub acp_session_id: SessionId,
    pub created_at: tokio::time::Instant,
    pub last_used_at: tokio::sync::RwLock<tokio::time::Instant>,
    pub in_flight: std::sync::Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug, thiserror::Error)]
pub enum NewError<E> {
    #[error("max active conversations reached")] LimitReached,
    #[error("session/new failed: {0}")]          Spawn(E),
}

#[derive(Debug, thiserror::Error)]
pub enum AcquireError {
    #[error("conversation busy")]      Busy,
    #[error("conversation not found")] NotFound,
}

/// Owned guard returned by `acquire_in_flight`; drop frees the slot.
pub struct InFlightGuard(tokio::sync::OwnedMutexGuard<()>);

#[derive(Clone)]
pub struct ConversationMap { /* Arc<RwLock<HashMap<String, Arc<Conversation>>>>, max_active, idle_window */ }

impl ConversationMap {
    pub fn new(max_active: u32, idle_window: std::time::Duration) -> Self { unimplemented!() }
    pub async fn get_or_create<F, E>(&self, id: &str, spawn: F)
        -> Result<(std::sync::Arc<Conversation>, bool), NewError<E>>
    where F: FnOnce() -> Result<SessionId, E> { unimplemented!() }
    pub async fn acquire_in_flight(&self, id: &str) -> Result<InFlightGuard, AcquireError> { unimplemented!() }
    pub async fn sweep_idle(&self) -> Vec<String> { unimplemented!() }
    pub async fn get(&self, id: &str) -> Option<std::sync::Arc<Conversation>> { unimplemented!() }
}
```

Implementation notes:
- Use `try_lock_owned()` on `in_flight` to convert `WouldBlock` into `AcquireError::Busy` without awaiting.
- `get_or_create` must double-check after upgrading from read- to write-lock to avoid spawning twice under concurrency.
- `sweep_idle` returns the evicted ids so the caller (idle reaper, Task 27) can issue `session/cancel` against the ACP Agent for each.

PASS. Commit `feat(serve): ConversationMap with H1 serial guard and idle sweep`.

### Task 17: `AcpClient` — spawn + initialize + session/new + session/prompt (spec §2.6, ADRs 0001/0002)

**Files:** `src/acp_client.rs`, `tests/acp_client_smoke.rs`; add `pub mod acp_client;`.

The Phase 0 reality check (V2) selected the exact form of the 0.13 API. This task wraps that decision behind a stable trait inside our crate so the rest of the code never depends on `agent-client-protocol` directly. The trait must be `async_trait`-free where possible — prefer `impl Future` returning methods on stable Rust if 0.13 exposes them; fall back to `async-trait` only if necessary.

- [ ] **Step 1 — Failing test** (requires Task 22 mock-acp-agent). It launches the mock binary, opens an `AcpClient`, calls `initialize`, calls `session/new`, sends a `session/prompt`, and asserts: (a) `initialize` returns the protocol version reported by the mock; (b) `session/new` returns a non-empty session id; (c) the returned `session/update` stream yields at least one `agent_message_chunk` and a terminal `stopReason`.

```rust
use a2a_shim_serve::acp_client::{AcpClient, AcpClientConfig};
use std::path::PathBuf;

#[tokio::test]
async fn smoke_initialize_new_prompt() {
    let mock = env!("CARGO_BIN_EXE_mock_acp_agent");
    let cfg  = AcpClientConfig { command: mock.into(), args: vec!["--script", "happy"].into(), cwd: std::env::temp_dir(), env: Default::default() };
    let mut cli = AcpClient::spawn(cfg).await.expect("spawn");
    let init = cli.initialize().await.expect("initialize");
    assert!(init.protocol_version >= 1);
    let sid = cli.session_new(PathBuf::from(std::env::temp_dir())).await.expect("session/new");
    assert!(!sid.is_empty());
    let mut stream = cli.session_prompt(&sid, "hello").await.expect("session/prompt");
    let mut saw_chunk = false; let mut stop_reason = None;
    while let Some(ev) = stream.next().await {
        use a2a_shim_serve::acp_client::SessionUpdate::*;
        match ev.expect("ok") {
            AgentMessageChunk { .. } => saw_chunk = true,
            StopReason(r)            => { stop_reason = Some(r); break; }
            _ => {}
        }
    }
    assert!(saw_chunk); assert!(stop_reason.is_some());
}
```

- [ ] **Step 2 — Implement** the trait + concrete client:

```rust
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AcpClientConfig {
    pub command: String, pub args: Vec<String>, pub cwd: PathBuf, pub env: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct InitializeResult { pub protocol_version: u32 /* + whatever 0.13 returns */ }

#[derive(Debug, Clone)]
pub enum SessionUpdate {
    AgentMessageChunk { text: String },
    AgentThoughtChunk { text: String },
    ToolCall          { id: String, name: String, input: serde_json::Value },
    ToolCallResult    { id: String, output: serde_json::Value },
    StopReason(StopReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason { EndTurn, Refusal, Cancelled, MaxTokens, ToolError }

#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("spawn failed: {0}")]    Spawn(#[source] std::io::Error),
    #[error("transport: {0}")]       Transport(String),
    #[error("agent error: {0}")]     Agent(String),
    #[error("permission rejected")]  PermissionRejected,
}

pub struct AcpClient { /* child handle, request/response plumbing from agent-client-protocol 0.13 */ }

impl AcpClient {
    pub async fn spawn(cfg: AcpClientConfig) -> Result<Self, AcpError> { unimplemented!() }

    /// ADR 0001: clientCapabilities.fs = false, terminal = false.
    pub async fn initialize(&mut self) -> Result<InitializeResult, AcpError> { unimplemented!() }

    /// ADR 0002: mcpServers = [].
    pub async fn session_new(&mut self, cwd: PathBuf) -> Result<String /* session_id */, AcpError> { unimplemented!() }

    pub async fn session_prompt(&mut self, session_id: &str, text: &str)
        -> Result<futures::stream::BoxStream<'_, Result<SessionUpdate, AcpError>>, AcpError> { unimplemented!() }

    pub async fn session_cancel(&mut self, session_id: &str) -> Result<(), AcpError> { unimplemented!() }
}
```

Implementation notes:
- The `agent-client-protocol = "0.13"` API surface is fluid; honor whatever the Phase 0 V2 finding says is the right entry point. If 0.13 exposes a typed `Connection`, hold one per `AcpClient` and translate notifications into `SessionUpdate`. If it exposes a lower-level codec, wrap it.
- All inbound `session/request_permission` calls are routed to `permission.rs` (Task 20). Implement that as a callback on `AcpClient` so this task ships without policy knowledge.
- All inbound `elicitation/create` calls are routed to `elicitation.rs` (Task 21) — same pattern.
- File descriptors: stdin/stdout piped, stderr inherited (the ACP Agent's stderr should appear in the operator's terminal under WARN, but never on our stdout).

PASS. Commit `feat(serve): AcpClient wrapping agent-client-protocol 0.13`.

### Task 18: `TaskRegistry` + `TaskBinding` state machine (spec §2.5, §2.6)

**Files:** `src/task_registry.rs`, `tests/task_registry.rs`; add `pub mod task_registry;`.

`TaskBinding` is the runtime object bridging one A2A `TaskId` to one ACP `session/prompt` cycle. It owns: `conversation_id`, `acp_session_id`, an `SseSink`, current `TaskState`, accumulated `history` (for `tasks/get` responses), accumulated `artifacts`, and an `IdleGuard` driven by ACP activity.

Allowed transitions (enforced):
- `Submitted → Working` (first ACP update arrives)
- `Working → Completed | Failed | Canceled` (terminal ACP update)
- `Working → InputRequired` (spec §2.5: Agent returns control without finishing)
- `InputRequired → Working` (continuation: client sends `message/send` with the same `id`)
- `* → Canceled` is permitted only from `Submitted | Working | InputRequired`; calling `tasks/cancel` on a terminal task returns `TASK_NOT_CANCELABLE` (-32002).

- [ ] **Step 1 — Failing test** covers each allowed transition, plus three rejections: continuation from `Working` is rejected (only from `InputRequired`); cancel from `Completed` returns `NotCancelable`; an unknown `TaskId` returns `NotFound`.

```rust
use a2a_shim_serve::task_registry::{TaskRegistry, TransitionError};
use a2a_shim_core::wire::task::{TaskId, TaskState};

#[tokio::test]
async fn submit_then_working_then_completed() {
    let reg = TaskRegistry::new();
    let id = reg.create("alice/review", "sess-1").await;
    reg.transition(&id, TaskState::Working).await.unwrap();
    reg.transition(&id, TaskState::Completed).await.unwrap();
    assert_eq!(reg.snapshot(&id).await.unwrap().status.state, TaskState::Completed);
}

#[tokio::test]
async fn continuation_only_from_input_required() {
    let reg = TaskRegistry::new();
    let id = reg.create("c", "s").await;
    reg.transition(&id, TaskState::Working).await.unwrap();
    assert!(matches!(reg.accept_continuation(&id).await, Err(TransitionError::InvalidContinuation)));
    reg.transition(&id, TaskState::InputRequired).await.unwrap();
    assert!(reg.accept_continuation(&id).await.is_ok());
    assert_eq!(reg.snapshot(&id).await.unwrap().status.state, TaskState::Working);
}

#[tokio::test]
async fn cancel_after_terminal_rejected() {
    let reg = TaskRegistry::new();
    let id = reg.create("c", "s").await;
    reg.transition(&id, TaskState::Working).await.unwrap();
    reg.transition(&id, TaskState::Completed).await.unwrap();
    assert!(matches!(reg.cancel(&id).await, Err(TransitionError::NotCancelable)));
}

#[tokio::test]
async fn snapshot_of_unknown_id_is_none() {
    assert!(TaskRegistry::new().snapshot(&TaskId::from("t-nope")).await.is_none());
}
```

- [ ] **Step 2 — Implement** the public surface:

```rust
use a2a_shim_core::wire::task::{Task, TaskId, TaskState};
use crate::sse_sink::SseSink;

pub struct TaskBinding {
    pub conversation_id: String,
    pub acp_session_id: String,
    pub sink: SseSink,
    /* state, history, artifacts, idle guard, created_at */
}

#[derive(Debug, thiserror::Error)]
pub enum TransitionError {
    #[error("task not found")]                NotFound,
    #[error("task not cancelable")]           NotCancelable,
    #[error("illegal transition")]            Illegal,
    #[error("continuation only valid from input-required")] InvalidContinuation,
}

#[derive(Clone, Default)]
pub struct TaskRegistry { /* Arc<RwLock<HashMap<TaskId, TaskBinding>>> */ }

impl TaskRegistry {
    pub fn new() -> Self { Self::default() }
    pub async fn create(&self, conversation_id: &str, acp_session_id: &str) -> TaskId { unimplemented!() }
    pub async fn transition(&self, id: &TaskId, to: TaskState) -> Result<(), TransitionError> { unimplemented!() }
    pub async fn accept_continuation(&self, id: &TaskId) -> Result<(), TransitionError> { unimplemented!() }
    pub async fn cancel(&self, id: &TaskId) -> Result<(), TransitionError> { unimplemented!() }
    pub async fn snapshot(&self, id: &TaskId) -> Option<Task> { unimplemented!() }
    pub async fn sink(&self, id: &TaskId) -> Option<SseSink> { unimplemented!() }
}
```

PASS. Commit `feat(serve): TaskRegistry with state-machine-enforced transitions`.

### Task 19: ACP→A2A bridge — `session/update` translation (spec §2.5, §2.11, §4.5)

**Files:** `src/bridge.rs`, `tests/bridge.rs`; add `pub mod bridge;`.

`bridge::run_session(...)` consumes an `AcpClient::session_prompt` stream and:
1. Resets the `IdleGuard` on every update.
2. Translates `AgentMessageChunk` / `AgentThoughtChunk` into appended `artifact-update` SSE events.
3. Accumulates text chunks into the canonical "answer" artifact (`artifactId = "a-answer"`, `append = true`).
4. On `StopReason::EndTurn` → transitions the Task to `Completed` and publishes the terminal `status-update`.
5. On `StopReason::Cancelled` → `Canceled`.
6. On `StopReason::Refusal | ToolError | MaxTokens` → `Failed`, with a `message` part describing the reason.
7. On stream error → `Failed`, normalized to spec §3.8 shape.
8. Updates `TaskBinding.history` and `artifacts` so a subsequent `tasks/get` returns a coherent snapshot.

- [ ] **Step 1 — Failing test** uses an in-memory `MockAcpClient` (a struct implementing the same trait via a `tokio::sync::mpsc::Receiver<SessionUpdate>`) and asserts: a sequence `[Chunk("hi"), Chunk(" there"), StopReason(EndTurn)]` produces SSE frames in this order: `status(working)`, `artifact-update(append=false, "hi")`, `artifact-update(append=true, " there")`, `status(completed, final=true)`. The final `TaskBinding.artifacts[0]` text is `"hi there"`.

```rust
// tests/bridge.rs — sketch
use a2a_shim_serve::bridge::{run_session, BridgeInputs};
use a2a_shim_serve::sse_sink::{SseFrame, SseSink};
use a2a_shim_serve::task_registry::TaskRegistry;
// + MockAcpStream helper that yields a scripted Vec<SessionUpdate>

#[tokio::test]
async fn happy_path_chunks_then_completed() {
    let reg = TaskRegistry::new();
    let task_id = reg.create("c", "s").await;
    let sink = reg.sink(&task_id).await.unwrap();
    let mut rx = sink.subscribe();

    let stream = MockAcpStream::new(vec![
        SessionUpdate::AgentMessageChunk { text: "hi".into() },
        SessionUpdate::AgentMessageChunk { text: " there".into() },
        SessionUpdate::StopReason(StopReason::EndTurn),
    ]);
    run_session(BridgeInputs { task_id: task_id.clone(), registry: reg.clone(), stream }).await.unwrap();

    let frames = drain_n(&mut rx, 4).await;
    assert!(matches!(frames[0], SseFrame::Event(SseEvent::StatusUpdate { status: TaskStatus { state: TaskState::Working, .. }, .. })));
    // artifact-update with append=false then append=true; final status-update with final=true
    let final_task = reg.snapshot(&task_id).await.unwrap();
    assert_eq!(final_task.status.state, TaskState::Completed);
    assert_eq!(extract_text(&final_task.artifacts[0]), "hi there");
}
```

- [ ] **Step 2 — Implement** `run_session` with the translation table above. Use `#[tracing::instrument(skip(inputs), fields(task_id = %inputs.task_id))]`.

- [ ] **Step 3 — Additional cases** (one test each):
  - `StopReason::Cancelled` → terminal `canceled` SSE with `final = true`, registry transitions to `Canceled`.
  - `StopReason::ToolError` → terminal `failed` SSE; `status.message` text part contains `"tool error"`.
  - Stream yields `Err(AcpError::Transport(_))` → terminal `failed` SSE; status message includes the transport error string.

PASS. Commit `feat(serve): bridge ACP session/update into A2A SSE`.

### Task 20: Permission policy (spec §2.7)

**Files:** `src/permission.rs`, `tests/permission.rs`; add `pub mod permission;`.

Three strategies (spec §2.7 + §2.3):

- `AutoApprove` — return `selected = "approve"` for every request; emit `tracing::warn!` on each decision.
- `AutoReject` — return `selected = "reject"` for every request.
- A second-axis `deny_tool_kinds: Vec<String>` — if the requested tool's `kind` matches any entry, force `reject` regardless of strategy.
- `Passthrough` — REJECTED at config load (Task 13). This task only handles the two MVP strategies.

- [ ] **Step 1 — Failing test** covers: `AutoApprove + deny=[]` → approve; `AutoApprove + deny=["delete"]` on a `delete` tool → reject; `AutoReject` always rejects; the `Approve` decision includes the `optionId` from the first option whose `kind = "allow_once"`; if no such option exists, fall back to the first option in the list.

```rust
use a2a_shim_serve::permission::{Decision, PermissionPolicy};
use a2a_shim_core::config::serve_toml::PermissionStrategy;

#[test]
fn auto_approve_with_no_deny_returns_approve() { /* ... */ }
#[test]
fn auto_approve_with_deny_for_matching_kind_returns_reject() { /* ... */ }
#[test]
fn auto_reject_always_rejects() { /* ... */ }
#[test]
fn approve_picks_allow_once_option_id() { /* ... */ }
#[test]
fn approve_falls_back_to_first_option_when_no_allow_once() { /* ... */ }
```

- [ ] **Step 2 — Implement** `PermissionPolicy::evaluate(request) -> Decision` as a pure function. Use it inside `AcpClient`'s `session/request_permission` callback (wired in Task 17 already).

PASS. Commit `feat(serve): permission policy with auto_approve/auto_reject + deny list`.

### Task 21: Elicitation handler — return `method-not-implemented` (spec §2.8)

**Files:** `src/elicitation.rs`, `tests/elicitation.rs`; add `pub mod elicitation;`.

`elicitation/create` from the ACP Agent has no human in front of a Serve Shim. MVP behavior: return JSON-RPC error `-32601` (Method Not Found) with a message naming the unsupported method. The active Task transitions to `Failed` with a status message identifying the cause.

- [ ] **Step 1 — Failing test**: calling `handle_elicitation(req, &registry, &task_id)` returns `Err(JsonRpcError { code: -32601, .. })` whose message contains `"elicitation"`; registry snapshot reports `Failed` with the elicitation reason embedded in `status.message`.
- [ ] **Step 2 — Implement** as a thin function; wire it into `AcpClient` callback table (Task 17).

PASS. Commit `feat(serve): elicitation method-not-implemented + Task transitions to Failed`.

### Task 22: `mock-acp-agent` — scripted stdio mock

**Files:** flesh out `tests/mock_acp_agent/Cargo.toml` and `src/main.rs`.

A standalone binary that speaks just enough ACP over stdio for Serve tests. Selected behavior via a `--script` flag:
- `happy` — returns one `agent_message_chunk` with `"4"` then `StopReason::EndTurn`.
- `slow` — sleeps 50 ms between two chunks before stopping.
- `refusal` — emits `StopReason::Refusal` immediately after a thought chunk.
- `tool-error` — emits a synthetic `session/request_permission`, then a `StopReason::ToolError`.
- `crash` — exits with code 1 mid-prompt.
- `noop-cancel` — after `session/cancel`, accepts a second `session/prompt` on the same `session_id` (validates V8 behavior end-to-end).

The mock MUST be conservative: it speaks the **0.13 wire format**. Build it on top of `agent-client-protocol = "0.13"` as a *server*, not as a hand-rolled JSON-RPC peer, so wire-shape drift fails the build.

- [ ] **Step 1 — Cargo.toml**: depends on `agent-client-protocol`, `tokio`, `serde_json`, `clap`. `[[bin]] name = "mock_acp_agent"`.
- [ ] **Step 2 — `main.rs`**: parse `--script`, instantiate an in-process ACP server bound to stdio, dispatch on the script value.
- [ ] **Step 3 — Unit test on the mock itself** (`tests/mock_acp_agent/tests/scripts.rs`): for `happy`, write the canonical `initialize` request to a `Stdio::piped()` child, read the response, assert `agent_message_chunk` arrives. This validates the mock before any Serve test uses it.

PASS. Commit `feat(test): mock-acp-agent with happy/slow/refusal/tool-error/crash/noop-cancel scripts`.

### Task 23: AgentCard generation (spec §2.10, §4.7)

**Files:** `src/agent_card.rs`, `tests/agent_card.rs`; add `pub mod agent_card;`.

`build_agent_card(cfg: &ServeConfig, bound_url: &str) -> AgentCard` populates the wire type from Task 10. `bound_url` precedence: `cfg.server.advertised_endpoint` if set, else `format!("http://{bound}/", bound)`.

- [ ] **Step 1 — Failing test** asserts:
  1. With `advertised_endpoint = None` and bound `127.0.0.1:7001`, `card.url == "http://127.0.0.1:7001/"`.
  2. With `advertised_endpoint = Some("https://x.example/")`, the card carries that URL verbatim.
  3. The card serializes to a JSON Object that exactly equals the spec §2.10 example for the canonical config (within reordering — assert field-by-field, not byte-equal).
  4. `card.metadata.conversations.{maxActive, idleSecs}` mirror `cfg.server.conversations`.
- [ ] **Step 2 — Implement**.

PASS. Commit `feat(serve): build_agent_card honoring advertised_endpoint and conversations capability`.

### Task 24: Axum router + GET `/.well-known/agent.json`

**Files:** `src/http.rs` (new), `tests/http_agent_card.rs`; add `pub mod http;`.

- [ ] **Step 1 — Failing test** boots the router on a random port via `tokio::net::TcpListener::bind("127.0.0.1:0")`, GETs `/.well-known/agent.json`, asserts HTTP 200, `Content-Type: application/json`, body parses as `AgentCard`, and `card.capabilities.streaming == true`.
- [ ] **Step 2 — Implement** `pub fn router(state: ServeState) -> axum::Router` and a handler `async fn agent_card_handler(State(s): State<ServeState>) -> impl IntoResponse`. `ServeState` is `Arc { config, conv_map, task_registry, acp_client_factory }`. The card path is read from config (default `/.well-known/agent.json`).

PASS. Commit `feat(serve): axum router + AgentCard endpoint`.

### Task 25: JSON-RPC root `POST /` — `message/send` (spec §2.6, §4.4)

**Files:** extend `src/http.rs`; new `tests/http_message_send.rs`.

Dispatch on `method`:
- `message/send` — synchronous Task creation or continuation.
- `message/stream` — SSE bridge (Task 26).
- `tasks/get` — `TaskRegistry::snapshot` → `Task` or `TASK_NOT_FOUND`.
- `tasks/cancel` — registry cancel + `AcpClient::session_cancel`; returns the post-cancel `Task` or `TASK_NOT_CANCELABLE`.
- anything else — `METHOD_NOT_FOUND`.

For `message/send`: extract `conversation_id` from `params.message.metadata["x-a2a-shim/conversation"]`; reject with `INVALID_PARAMS` if missing; `get_or_create` the conversation (spawning ACP via factory if new); `acquire_in_flight` or return `CONVERSATION_BUSY`; create a Task; run the bridge; on terminal state, return the final `Task` snapshot as the JSON-RPC result. On `LimitReached` → `CONVERSATION_LIMIT_REACHED`.

- [ ] **Step 1 — Failing tests** (one per branch — write them all up front, watch all fail):
  - happy path against mock `happy` returns `result.status.state == "completed"` and a single artifact whose text is `"4"`.
  - missing `x-a2a-shim/conversation` metadata → JSON-RPC error code `-32602`.
  - second concurrent `message/send` on the same `conversation_id` while the first is in flight → `-32010` (`CONVERSATION_BUSY`).
  - exceeding `max_active = 1` with a different `conversation_id` while the first is in flight → `-32011`.
  - continuation: `id = "t-existing"` against a Task currently in `input-required` works; against a Task in `working` returns `-32602` with `InvalidContinuation` text.
  - `tasks/get` on unknown id → `-32001`.
  - `tasks/cancel` after terminal → `-32002`.
  - `method: "frobnicate"` → `-32601`.
- [ ] **Step 2 — Implement** the dispatcher and individual method handlers. Use one `axum::Json<JsonRpcRequest<Value>>` extractor and one `axum::Json<JsonRpcResponse<Value>>` responder; map every internal error type to a `JsonRpcError` via a small `From` impl chain in `error/normalize.rs`.

PASS. Commit `feat(serve): JSON-RPC root with message/send, tasks/get, tasks/cancel`.

### Task 26: SSE `message/stream` endpoint (spec §2.6, §4.5)

**Files:** extend `src/http.rs`; new `tests/http_message_stream.rs`.

For `message/stream`: create the Task as in Task 25, then return an `axum::response::sse::Sse<…>` body driven by `SseSink::subscribe()`. On `SseFrame::Event`, render via `encode_sse_event`; on `SseFrame::Keepalive`, emit `: keepalive\n\n`. On `RecvError::Lagged`, end the stream with a `failed` synthetic event referencing the spec §3.8 normalized error (kind `protocol_error`).

- [ ] **Step 1 — Failing test** uses `reqwest::Client::get` with `eventsource-stream` to consume the response from the mock `happy` script. It asserts events arrive in order: at least one `status-update working`, an `artifact-update`, then a `status-update completed final=true`, then EOF within 2 s.
- [ ] **Step 2 — Implement**. Use `start_keepalive` per Task 15.
- [ ] **Step 3 — Add a `slow` test** that uses paused virtual time to confirm a `: keepalive\n\n` line is emitted every 30 s when no events are flowing.

PASS. Commit `feat(serve): SSE message/stream with keepalive`.

### Task 27: `serve::run` wiring + idle reaper + shutdown

**Files:** `src/run.rs`, `src/idle_sweep.rs`, `src/shutdown.rs`; wire into `main.rs`.

`run(opts: ServeOpts) -> Result<()>`:
1. Load config (TOML file or pure-default `[agent]` skeleton — error if neither and no `--spawn`).
2. Apply CLI overrides (`--listen`, `--advertised-endpoint`, `--spawn`, `--cwd`, `--permission-strategy`, `--log-file`).
3. Init tracing via `logging::try_init`.
4. If the listener is non-loopback, log `WARN` with the exact text from spec §6.2.
5. Spawn idle reaper: every `idle_secs / 4` (clamp 30s..3600s), call `conv_map.sweep_idle()`; for each evicted id, call `acp_client.session_cancel`.
6. Bind `TcpListener`, build router, run `axum::serve(...).with_graceful_shutdown(shutdown_signal())`.
7. `shutdown_signal` waits for SIGINT/SIGTERM (Ctrl-C on Windows). On signal, set a shared `AtomicBool`; the reaper checks it on each loop and exits.

- [ ] **Step 1 — Failing test** (`tests/serve_run_smoke.rs`): spawn the full `a2a-shim serve` process pointed at the `mock-acp-agent`, listening on a random port; POST one `message/send`; assert the response carries `status.state = "completed"`. Use `tokio::process::Command::kill_on_drop(true)` to clean up.
- [ ] **Step 2 — Implement** every wiring step; replace the placeholder in `main.rs::Serve` arm.
- [ ] **Step 3 — Test the non-loopback warning** by binding `0.0.0.0:0` and asserting the log line (capture via `tracing_subscriber::fmt::TestWriter`).

PASS. Commit `feat(serve): wire serve::run with idle reaper and graceful shutdown`.

**Phase 2 exit gate:** `cargo test --workspace` is green and `target/debug/a2a-shim serve --config sample.toml` boots against the real `claude-agent-acp`. Manual smoke: `curl http://127.0.0.1:7001/.well-known/agent.json | jq .capabilities` shows `streaming: true`. Commit count: ~26.

---

## Phase 3 — Client Shim

End state: `a2a-shim client` runs as a stdio MCP server exposing one tool, `a2a_send`. The tool synchronously calls a remote A2A endpoint, consumes its SSE stream end-to-end (Sync-over-Stream, ADR 0003), emits MCP `notifications/progress` every 30 s while waiting, and returns the final `Task` (or normalized error) as the tool result. Cancellation honored per spec §3.9.

Discipline reminder: in this crate **stdout is the MCP transport**. Logs go to stderr or `--log-file`. Any accidental `println!` is a defect that will corrupt the wire — `cargo test -p a2a-shim-client` MUST include a smoke that asserts stdout contains only MCP JSON frames.

### Task 28: `a2a_send` tool schema (spec §3.2, §3.3)

**Files:** create `crates/a2a-shim-client/src/tool_schema.rs`, `tests/tool_schema.rs`; add `pub mod tool_schema;`.

The `tools/list` response advertises one tool. Its JSON Schema input has the following required fields (spec §3.3): `endpoint` (string, URL), `conversation_id` (string), `message` (string, Markdown allowed); optional: `task_id` (string, used to resume an `input-required` Task), `timeout_secs` (number, defaults to client `stream_idle_secs`), `metadata` (object, merged into outbound A2A `Message.metadata`).

- [ ] **Step 1 — Failing test** asserts: `tool_definition().name == "a2a_send"`; description mentions `"conversation"` and `"streaming"`; `input_schema.required` is exactly `["endpoint","conversation_id","message"]`; `input_schema.properties.endpoint.format == "uri"`; `input_schema.additionalProperties == false`; round-trip via `serde_json::to_value` then `from_value` yields equal definitions.

- [ ] **Step 2 — Implement** `pub fn tool_definition() -> McpToolDefinition` returning a typed struct (defined here) that serializes to the MCP `tools/list` shape. Use `serde_json::json!` for the schema literal — its byte-exact shape is part of the public contract with Hosts.

PASS. Commit `feat(client): a2a_send tool schema`.

### Task 29: MCP stdio server loop (spec §3.1, §3.4)

**Files:** `src/mcp_server.rs`, `tests/mcp_loop.rs`; add `pub mod mcp_server;`.

If Phase 0 V2 chose `agent-client-protocol::mcp_server`, use it. Otherwise hand-roll: read NDJSON from stdin via `tokio::io::BufReader<Stdin>::lines()`; dispatch on `method`:
- `initialize` — return server info, protocol version, and `capabilities.tools.listChanged = false`.
- `tools/list` — return `[tool_definition()]`.
- `tools/call` with `name = "a2a_send"` — dispatch to the call handler (Task 32).
- `notifications/cancelled` — see Task 32; cancellation pipe must be wired here.
- everything else — `-32601`.

Writes to stdout MUST be one JSON object per line, flushed after each write. Use a single writer task with a `tokio::sync::mpsc::Sender<Value>` shared by all handlers so concurrent `notifications/progress` from the heartbeat task can interleave with the eventual `tools/call` result without races or partial lines.

- [ ] **Step 1 — Failing test** spawns the client as a subprocess with `Stdio::piped()`, writes a canonical `initialize` request, reads exactly one line of JSON from stdout, asserts the response carries `result.capabilities.tools` and that stderr is empty of JSON-looking lines.
- [ ] **Step 2 — Failing test #2**: after init, sends `tools/list`; asserts the single tool is named `a2a_send`.
- [ ] **Step 3 — Implement** the loop.

PASS. Commit `feat(client): MCP stdio server loop with initialize and tools/list`.

### Task 30: Outbound A2A — JSON-RPC `message/stream` over HTTP/SSE (spec §3.6)

**Files:** `src/outbound.rs`, `tests/outbound.rs`; add `pub mod outbound;`.

`pub async fn stream(endpoint, conversation_id, message, task_id, deadlines) -> impl Stream<Item = Result<SseEvent, OutboundError>>` POSTs a JSON-RPC `message/stream` request with `Accept: text/event-stream` and consumes the response body via `eventsource-stream`. Parses each `data:` line as `SseEvent` (Task 9). Maps reqwest errors and HTTP non-2xx codes into `OutboundError` variants aligned to spec §4.6 / §3.8: `NetworkError`, `RemoteFailed`, `RemoteTimeout`, `ProtocolError`.

`deadlines: OutboundDeadlines { connect: Duration, stream_idle: Duration, hard_ceiling: Duration }`. The stream wrapper tracks an `IdleGuard` and a `HardCeiling`; if either trips, it yields `Err(RemoteTimeout)` and ends.

- [ ] **Step 1 — Failing test** uses `wiremock` (add to `dev-dependencies`) to spin up a fake A2A endpoint that emits two SSE events and then closes. The test asserts:
  1. Both events arrive and parse as the expected `SseEvent` variants.
  2. On HTTP 500 from the fake, the stream yields `RemoteFailed`.
  3. With a 50 ms `stream_idle` and a fake that sends one event then sleeps 200 ms, the stream yields `RemoteTimeout`.
- [ ] **Step 2 — Implement**.

PASS. Commit `feat(client): outbound A2A streaming with idle + hard-ceiling guards`.

### Task 31: Progress heartbeat (ADR 0003, spec §3.7)

**Files:** `src/heartbeat.rs`, `tests/heartbeat.rs`; add `pub mod heartbeat;`.

`Heartbeat::start(writer, progress_token, interval) -> HeartbeatGuard` spawns a tokio task that every `interval` posts an MCP `notifications/progress` with `params = { progressToken, progress: <monotonic counter>, total: null, message: <last summary> }`. `update_summary(text)` mutates the message used by the next tick (so the heartbeat reflects the most recent ACP activity, e.g. `"streaming chunk 14"`). Dropping the guard cancels the task.

If `progress_token` is `None` (Host did not send `_meta.progressToken`), `start` returns a no-op guard that never writes anything. This honors V4/V5 DEFERRED outcomes.

- [ ] **Step 1 — Failing test** uses paused virtual time + an in-memory `Vec<Value>` "writer" to assert: with a 1-second interval and a 3.5-second simulated wait, exactly 3 `notifications/progress` frames are written, all carrying the supplied `progressToken`, with monotonically increasing `progress` values; after `update_summary("step 2")`, the next frame's `message == "step 2"`.
- [ ] **Step 2 — Implement**.
- [ ] **Step 3 — Failing test #2**: with `progress_token = None`, no frames are written regardless of elapsed time.

PASS. Commit `feat(client): notifications/progress heartbeat per ADR 0003`.

### Task 32: `a2a_send` call handler — orchestration + cancellation (spec §3.5–3.9)

**Files:** `src/render.rs`, `src/cancellation.rs`; extend `mcp_server.rs` to wire them; new `tests/call_handler.rs`.

`call_a2a_send(args: A2aSendArgs, cx: CallContext) -> McpToolResult`:
1. Validate args via `serde` (rejecting the call with MCP error `-32602` if invalid).
2. Register the in-flight call in `CancellationRegistry` keyed by the inbound MCP request id.
3. Start the progress heartbeat (Task 31) with `cx.progress_token`.
4. Open the outbound stream (Task 30).
5. Drive the stream, updating the heartbeat summary on each event. When a terminal `status-update` arrives (`final = true`), build the final `Task` snapshot.
6. Render the result via `render::render_tool_result(task)` — see below.
7. If the cancellation registry signals during steps 4–5, send A2A `tasks/cancel` to the remote (best-effort, ignore errors), then return the MCP-canonical "request cancelled" error result.

`render_tool_result(task)` — spec §3.5:
- On `state = "completed"`: MCP result `content = [{ type: "text", text: <concatenated artifact texts> }]` plus `_meta = { a2aTask: <full Task JSON> }`.
- On `state = "failed" | "canceled"` or any normalized error: MCP result with `isError: true`, `content = [{ type: "text", text: <human-readable error> }]`, `_meta = { error: NormalizedError }`.
- On `state = "input-required"`: MCP result is **not** an error; `content` carries the agent's last message text, `_meta = { taskId, state: "input-required" }`, with a hint string telling the caller to resume via a new `a2a_send` carrying `task_id`.

`CancellationRegistry`:
```rust
pub struct CancellationRegistry { /* Mutex<HashMap<RequestId, tokio_util::sync::CancellationToken>> */ }
impl CancellationRegistry {
    pub fn register(&self, id: RequestId) -> tokio_util::sync::CancellationToken { unimplemented!() }
    pub fn cancel(&self, id: &RequestId) { unimplemented!() }
    pub fn unregister(&self, id: &RequestId) { unimplemented!() }
}
```
The MCP loop (Task 29) calls `cancel(id)` on `notifications/cancelled`. The call handler drops its registration in a `defer`-style guard.

- [ ] **Step 1 — Failing tests** (build them all before implementing):
  - Happy path: against a wiremock A2A endpoint that streams `working` → `artifact-update("ok")` → `completed`, the tool result has `isError = false`, content text `"ok"`, `_meta.a2aTask.status.state == "completed"`.
  - `RemoteTimeout` from outbound: result has `isError = true`, `_meta.error.kind == "remote_timeout"`.
  - `input-required` terminal state: result is **not** an error, `_meta.state == "input-required"`, `_meta.taskId` non-empty.
  - Cancellation: client receives `notifications/cancelled` for the in-flight request; the test fake records that an A2A `tasks/cancel` was POSTed against the remote within 500 ms, and the tool result reports cancelled.
  - Heartbeat interleave: during a 5-second wiremock stream with `heartbeat_secs = 1`, the test parses the client's stdout and confirms at least 3 `notifications/progress` lines arrived strictly before the final `tools/call` response line, all with monotonically increasing `progress`.
- [ ] **Step 2 — Implement** the handler, `render`, and `cancellation`. Use `tokio::select!` to race the outbound stream against the cancellation token.

PASS. Commit `feat(client): a2a_send orchestration with rendering, heartbeat, cancellation`.

### Task 33: `client::run` wiring + stdout/stderr discipline check

**Files:** `src/run.rs`; wire into `main.rs`.

`run(opts: ClientOpts) -> Result<()>`:
1. Init tracing — destination MUST be `LogDestination::File(opts.log_file)` if set, else `LogDestination::Stderr`. Never stdout. The `try_init` helper rejects a `LogDestination::Stdout` because no such variant exists.
2. Build `ClientState { connect: opts.connect_timeout_secs, stream_idle: opts.stream_idle_secs, hard_ceiling: opts.hard_ceiling_secs, heartbeat: opts.heartbeat_secs, http: reqwest::Client::builder().connect_timeout(...).build() }`.
3. Run `mcp_server::serve(stdin, stdout, state).await`.

- [ ] **Step 1 — Failing test** (`tests/client_run_smoke.rs`): spawn `a2a-shim client --log-file /tmp/a2a-shim-client.log`, send `initialize`, send `tools/list`, send a `tools/call` against a wiremock endpoint, assert stdout contains exactly three JSON object lines (response, response, response) plus optional `notifications/progress` lines, **and zero non-JSON lines on stdout**.
- [ ] **Step 2 — Implement** and replace the placeholder in `main.rs::Client` arm.

PASS. Commit `feat(client): wire client::run with stderr-only logging discipline`.

**Phase 3 exit gate:** `cargo test --workspace` is green. Manual smoke: start `a2a-shim serve` against the mock, point `a2a-shim client` at it via a hand-crafted `tools/call`, observe the full streaming round-trip on stderr logs and a clean `Task` on stdout. Commit count: ~32.

---

## Phase 4 — Integration & Hardening

End state: an end-to-end test exercises Host → Client Shim → A2A → Serve Shim → ACP Agent → A2A → Client Shim → Host using two `a2a-shim` processes plus the real `claude-agent-acp`. Workspace lints clean. README and sample config in repo.

### Task 34: End-to-end self-loopback integration test

**Files:** `crates/a2a-shim/tests/e2e_self_loopback.rs`; create `sample-config.toml`.

The test:
1. Writes `sample-config.toml` pointing `[agent].command = mock_acp_agent` with `--script happy`.
2. Spawns `a2a-shim serve --config sample-config.toml --listen 127.0.0.1:0`. Parses the bound port from stderr (`run.rs` MUST log `serve listening on <bind>` at INFO).
3. Spawns `a2a-shim client` as a stdio child.
4. Sends `initialize`, then `tools/call { name: "a2a_send", arguments: { endpoint: "http://127.0.0.1:<port>", conversation_id: "e2e/loopback", message: "what is 2+2?" } }`.
5. Asserts the tool result text equals `"4"` and `_meta.a2aTask.status.state == "completed"`.
6. Kills both children via `kill_on_drop`.

Marked `#[ignore]` only if it cannot run reliably under CI; default to running.

- [ ] **Step 1 — Write the test**, watch it fail.
- [ ] **Step 2 — Fix any wiring bugs surfaced** (do not regress earlier tests).
- [ ] **Step 3 — Commit** `test(e2e): self-loopback through both shims and mock ACP`.

### Task 35: AgentCard discovery test through real `claude-agent-acp`

Optional, conditional on Phase 0 V1 PASS.

- [ ] **Step 1 — `tests/e2e_claude_agent_acp.rs`** wired exactly like Task 34 but with `[agent].command = "claude-agent-acp"`. Skip with `#[ignore]` if `ANTHROPIC_API_KEY` is unset, but if set, run a 1-turn prompt and assert `state == "completed"` plus a non-empty artifact text.
- [ ] **Step 2 — Document** in README how to enable: `cargo test --ignored e2e_claude_agent_acp`.
- [ ] **Step 3 — Commit** `test(e2e): real claude-agent-acp end-to-end (ignored without API key)`.

### Task 36: Lint and format hygiene

- [ ] **Step 1 — Run** `cargo fmt --all -- --check`. Fix and re-run until clean.
- [ ] **Step 2 — Run** `cargo clippy --workspace --all-targets -- -D warnings`. Fix every warning. No `#[allow(...)]` without a `// SAFETY:` or `// REASON:` comment.
- [ ] **Step 3 — Run** `cargo test --workspace`. All green.
- [ ] **Step 4 — Commit** `chore: fmt + clippy clean across workspace`.

### Task 37: README and sample config

**Files:** create `README.md`, `sample-config.toml`, `docs/operating-notes.md`.

- [ ] **Step 1 — `README.md`** sections:
  1. What A2A-Shim is (two sentences, link to spec).
  2. Install: `cargo build --release`.
  3. Serve quickstart: `a2a-shim serve --config sample-config.toml`, expected AgentCard `curl` output.
  4. Client quickstart: how a Host (Claude Code) registers the client as an MCP server.
  5. Limits: single in-flight prompt per conversation; auto_approve default; loopback-only default; no auth.
  6. Links to ADRs and the spec.
- [ ] **Step 2 — `sample-config.toml`** identical to spec §5.2 canonical example.
- [ ] **Step 3 — `docs/operating-notes.md`**: how to put the Serve Shim behind a port-forward / SSH tunnel; how to enable JSON logs (`--log-format json`); how to read `tracing` spans for correlation by `task_id`.
- [ ] **Step 4 — Commit** `docs: README + sample config + operating notes`.

### Task 38: Cut v0.1.0 tag

- [ ] **Step 1 — Final check**: `cargo build --release --workspace && cargo test --workspace` clean.
- [ ] **Step 2 — Update CHANGELOG** (create if missing): one section `## [0.1.0] — <today>` enumerating the four phases and link to the spec.
- [ ] **Step 3 — Commit** `chore(release): v0.1.0`.
- [ ] **Step 4 — Tag**: `git tag -a v0.1.0 -m "A2A-Shim MVP"`.

**Phase 4 exit gate:** end-to-end test green; workspace lints clean; README explains how a new operator can stand up both shims in under five minutes. Commit count: ~37 + tag `v0.1.0`.

---

## Plan-Wide Risk Register

| # | Risk | Surfaces in | Mitigation |
|---|------|-------------|------------|
| R1 | `agent-client-protocol = "0.13"` API shifts mid-implementation | Tasks 17, 22 | Phase 0 spike pins the actual API; isolate behind `AcpClient` trait so the rest of the code is insulated. |
| R2 | Claude Code does not send `_meta.progressToken` on `tools/call` | Tasks 31, 32 | Heartbeat module degrades to no-op; Phase 0 V4/V5 either PASS or DEFERRED, never block. |
| R3 | `wiremock` SSE support insufficient for Phase 3 tests | Tasks 30, 32, 34 | Fallback to a hand-rolled `axum` server inside the test crate that serves the canned SSE bytes. Decide during Task 30 — do not let it block Phase 3. |
| R4 | Windows signal handling differs from spec §6.5 assumptions | Task 27 | `tokio::signal::ctrl_c()` is cross-platform; SIGTERM is Unix-only. On Windows, accept only Ctrl-C and document this in `docs/operating-notes.md`. |
| R5 | Plan reference to spec line numbers drifts after spec edits | All | Tasks cite section numbers (`§2.6`), not line numbers, in code. If a section is renumbered, this plan is amended in a single commit before next task. |

---

## Final Checklist Before Calling Plan "Done"

- [ ] Spec, ADRs, and CONTEXT.md are unchanged or all amendments committed before coding.
- [ ] Phase 0 REPORT.md committed; no blocking FAIL.
- [ ] `cargo build --workspace --release` succeeds.
- [ ] `cargo test --workspace` succeeds (including ignored tests when `ANTHROPIC_API_KEY` is set).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` succeeds.
- [ ] `cargo fmt --all -- --check` succeeds.
- [ ] End-to-end test `e2e_self_loopback` is green and **not** marked `#[ignore]`.
- [ ] No `unimplemented!()`, `todo!()`, or `unreachable!()` outside genuine "this branch is impossible" guards with a `// REASON:` comment.
- [ ] README walks an operator from `cargo build` to a working Host→Specialist call in under five minutes.
- [ ] `v0.1.0` tag pushed.
