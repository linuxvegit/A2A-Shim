# ADR 0007 — Conversation & Task Persistence via SQLite

**Date:** 2026-06-04
**Status:** Accepted
**Relates to:** v1.1 item #3, ADR 0004 (caller_id partitioning to land
on item #4).
**Supersedes:** "In-memory ConversationMap + TaskRegistry" stance
documented in v0.1.0 spec § 2.13 and CONTEXT.md's
"best-effort hardening" note.

## Context

v0.1.0 kept `ConversationMap` and `TaskRegistry` in memory. A Serve
Shim restart lost every active conversation and task; the operator
guidance was "supervisor restarts daily, set `idle_secs` long enough
that loss is rare." This was acceptable for the MVP but fails the spec's
own "best-effort continuity" hardening promise the moment any operator
cares about state surviving a deploy.

v1.1 Phase 0 Spike A made the full restart-resume story tractable in
three findings:

1. `agent-client-protocol-schema = 0.13.5` ships `LoadSessionRequest` /
   `ResumeSessionRequest` and the matching capability flags.
2. `claude-agent-acp@0.40.0` advertises **both** `load_session: true`
   and `session_capabilities.resume: Some(_)` and answers the calls.
3. **Critically:** on `session/load`, the agent replays prior turns as
   `session/update` notifications. We do NOT need to persist
   `Task.history` or `Task.artifacts` — the agent reconstitutes them
   for us when we re-attach.

This third finding narrows the persistence surface dramatically. We
need only enough state to re-issue `session/load` for each
conversation and answer forensic `tasks/get` for terminal tasks.

## Decision

### Engine: `rusqlite` + `tokio::task::spawn_blocking`

- Synchronous SQLite via `rusqlite`, with every DB call wrapped in
  `spawn_blocking` to keep the tokio runtime free.
- One global `parking_lot::Mutex<rusqlite::Connection>` per Serve Shim
  process (SQLite serializes writers anyway; the mutex prevents
  re-entrant access from confusing the connection state).
- Embedded `CREATE TABLE IF NOT EXISTS …` strings; migrations via a
  small `_schema_version` table + hand-rolled per-version arms (no
  external migration crate dependency).

### Schema (initial v1)

```sql
CREATE TABLE _schema_version (version INTEGER NOT NULL PRIMARY KEY);
INSERT INTO _schema_version VALUES (1);

CREATE TABLE conversations (
    conversation_id   TEXT NOT NULL PRIMARY KEY,
    acp_session_id    TEXT NOT NULL,
    cwd               TEXT NOT NULL,
    caller_id         TEXT NOT NULL DEFAULT 'anonymous',
    created_at        INTEGER NOT NULL,   -- unix epoch ms
    last_used_at      INTEGER NOT NULL
);

CREATE TABLE tasks (
    task_id           TEXT NOT NULL PRIMARY KEY,
    conversation_id   TEXT NOT NULL REFERENCES conversations(conversation_id) ON DELETE CASCADE,
    state             TEXT NOT NULL,        -- TaskState as kebab-case
    created_at        INTEGER NOT NULL,
    terminal_at       INTEGER                -- NULL while non-terminal
);

CREATE TABLE push_notification_configs (
    config_id         TEXT NOT NULL PRIMARY KEY,
    task_id           TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
    url               TEXT NOT NULL,
    token             TEXT,                  -- nullable
    auth_scheme       TEXT,                  -- nullable
    auth_credentials  TEXT,                  -- nullable; secret
    tenant            TEXT,
    created_at        INTEGER NOT NULL
);

CREATE INDEX idx_tasks_conversation ON tasks(conversation_id);
CREATE INDEX idx_push_configs_task ON push_notification_configs(task_id);
```

`caller_id` is reserved on `conversations` for item #4 even though
the partitioning logic lands later — adding the column up front avoids
a schema migration for what is a known-pending change.

Notably absent:
- `Task.history` and `Task.artifacts` — reconstructed from the
  `session/load` replay.
- `Message` rows. We do not persist user prompts; they live in the
  ACP agent's own state.
- Anything from the AgentCard. The card is rendered fresh on every
  `GET /.well-known/agent.json` from `ServeConfig`.

### Configuration

```toml
[server.persistence]
enabled = true                          # default ON in v1.1
path = "./a2a-shim.db"                  # SQLite file path
```

`enabled = false` keeps v0.1.0's purely in-memory behavior. v1.1 ships
with the on-disk path as default so the spec's "best-effort hardening"
promise becomes "actually delivered" out of the box.

### Restart recovery

On Serve Shim start, if `[server.persistence].enabled` is true and the
DB file exists:

1. Spawn the ACP agent (unchanged from v0.1.0).
2. Read `conversations` rows from the DB.
3. For each row: issue `session/load(session_id, cwd)` against the
   newly-spawned agent.
4. On success: re-insert the conversation into the in-memory
   `ConversationMap` so the next `SendMessage` hits the cache.
5. On `session/load` failure (agent doesn't recognize the id, ID
   was provisioned against a different agent binary, etc.):
   `tracing::warn` and DELETE the row from `conversations`. The
   conversation is gone; future `SendMessage` for that id either
   creates a fresh session (mode=`auto`/`new`) or returns
   `ConversationLost` (mode=`continue`, item #5).
6. The bootstrap is concurrent-bounded at 8 in-flight `session/load`s
   to avoid hammering the agent on a large restore.

Tasks are NOT restored to live `TaskRegistry` on startup — they were
all in non-terminal state at restart, and the corresponding ACP work
was killed when the previous process died. The Task rows persist for
forensic `GetTask` so a Host that polls a saved `task_id` after restart
gets a coherent "this task was active when the shim restarted; its
last known state was X" response. They are NOT auto-resumed.

### Write paths

| Event | DB write |
|---|---|
| New conversation created | INSERT `conversations` |
| ConversationMap hit (touch `last_used_at`) | UPDATE `conversations SET last_used_at = ?` — every N (default 10) hits, NOT every hit, to bound write amplification |
| Conversation evicted by idle reaper | DELETE row (cascades to tasks + push configs) |
| Task created | INSERT `tasks(state='submitted', terminal_at=NULL)` |
| Task transitions | UPDATE `tasks SET state = ?` |
| Task reaches terminal state | UPDATE `tasks SET state = ?, terminal_at = ?` |
| PushNotificationConfig created (item #6) | INSERT `push_notification_configs` |
| PushNotificationConfig deleted (item #6) | DELETE row |

All writes happen in `spawn_blocking` and return `oneshot` to the
caller. The bridge does not await DB writes mid-stream — they fire
and forget with errors logged at `error!`. A DB write failure does NOT
fail the Task: we degrade to in-memory-only for that conversation/task
and log loudly.

### Schema migration

`_schema_version` table tracks the applied version (1 today). On
startup, if the table is missing or version < CURRENT, run the matched
`upgrade_to_v2`, `upgrade_to_v3`, … functions in order. Each upgrade is
an idempotent function `fn(&mut Connection) -> Result<()>` that
executes its DDL inside a transaction and bumps `_schema_version` on
success.

No external migration library. Each upgrade fn lives next to the
schema string it advances.

## Consequences

- v1.1 adds one workspace dep (`rusqlite` with the `bundled` feature so
  SQLite is statically linked; no system libsqlite3 dependency).
- The Serve Shim's in-memory state continues to be the source of truth
  for live operations; SQLite is the persisted projection.
- A corrupt or unreadable DB file fails Serve startup with a clear
  error. Operators can `rm a2a-shim.db` to start fresh (losing
  conversations) or pin `[server.persistence].enabled = false` to
  bypass.
- Cross-platform file-locking: SQLite handles its own locking; we do
  not need OS-specific code. Confirmed on Windows (project's primary
  development platform) during Spike A.
- v1.2 may add `session_states` row to record the last known
  `SessionMode` returned in the `session/load` response, so we can
  re-apply the user's preferred mode on restart. Not in v1.1 scope.

## Alternatives Considered

- **`sqlx`.** Compile-time SQL validation is appealing, but requires
  either a live DB at build time or offline mode with a checked-in
  `sqlx-data.json`. Adds CI friction and an extra binary dep. Rejected.
- **`redb` (pure-Rust KV store).** Zero C deps, smaller bundle, but
  loses SQL queryability — listing tasks would mean scanning. Rejected
  because the spec already calls out `ListTasks` as a v1.0 method (item
  #0.3) and we want pagination later.
- **Persist `Task.history` and `Task.artifacts` too.** Rejected because
  Spike A confirmed `session/load` replays the history. Persisting both
  would double the write volume and create reconciliation puzzles if the
  agent's replay disagrees with our stored copy.

## Implementation Notes

- DB-touching code lives in a new module
  `a2a_shim_serve::persistence` with the `parking_lot::Mutex<Connection>`
  hidden behind a small typed API:
  `Persistence::insert_conversation`, `Persistence::touch_conversation`,
  `Persistence::record_task_transition`, etc.
- Spawn cost on restart: 8 concurrent `session/load`s on a moderate
  load (say 50 conversations) means ~7 batches. Per-call cost depends
  on the agent; for `claude-agent-acp` Spike A measured ~1s. Operators
  with thousands of conversations should expect a 10-30s startup
  delay.
- `[server.persistence].enabled = false` is the canonical "I want
  v0.1.0 behavior" knob; documented in operating-notes.md (v1.1
  update).
