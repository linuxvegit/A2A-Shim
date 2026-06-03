# A2A-Shim — Domain Glossary

This file is a glossary of canonical domain terms used in the A2A-Shim
project. It is **not** a specification, scratchpad, or implementation
journal. When a term is resolved during design or grilling, it is added
here. When code uses a term, that term should match this file.

If you need to record an architectural decision, write an ADR under
`docs/adr/` instead.

---

## Conversation

A **caller-declared, contiguous series of `a2a_send` calls that belong to
the same continuation of context.** Same conversation = the remote agent
is expected to remember prior turns; different conversation = the remote
agent treats the call as independent.

**Identity.** A `conversation_id` is a string supplied by the caller via
the `conversation` argument of `a2a_send`. It is carried on the wire in
`message.metadata["x-a2a-shim/conversation"]` and mirrored to the A2A
standard field `task.context_id`.

**Defaulting rule.** If the caller omits the `conversation` argument, the
shim generates a fresh unique id for that call — so the **default is
isolation, not sharing.** This prevents unrelated calls from accidentally
sharing a remote agent's memory.

**`conversation_id` lives in a shared, unpartitioned namespace.** The
`Serve Shim` maintains a single global `ConversationMap` keyed solely
by the `conversation_id` string. It does **not** partition by `Caller`
identity — and cannot, because every inbound connection arrives via
the port-forwarding layer and appears to originate from a local
address. Two unrelated `Caller`s using the same `conversation_id` will
silently share an ACP session and pollute each other's context.

**Naming rule for `Caller`s.** Choose `conversation_id` values that are
globally unique under realistic operating assumptions. Recommended:

- A UUID (`"sess-7f3a8b2c-..."`)
- A caller-prefixed slug (`"alice/review-2026-06-03"`,
  `"ci-pipeline/job-9831"`)
- Any string that includes the `Caller`'s identity or a random suffix

Avoid:

- Short generic words (`"review"`, `"chat"`, `"work"`, `"session"`)
- Topic-only ids without caller scoping (`"db-schema"`,
  `"api-design"`) — these collide whenever any other `Caller` happens
  to pick the same topic name

**Reserved id `"default"`.** The literal string `"default"` is a
reserved opt-in for "deliberately reuse the default slot on this port"
in single-`Caller` deployments. In any multi-`Caller` environment,
`Caller`s MUST NOT use `"default"` — it is the most extreme case of
the collision risk above.

v1.1 will add an optional `caller_id` argument to `a2a_send` so the
`Serve Shim` can partition by `(caller_id, conversation_id)` when
`Caller`s elect to identify themselves. Until then, naming discipline
is the only defense.

**Relationships.**

- `Conversation` 1:1 ↔ one ACP `sessionId` on the serve side (lifetime
  tracked in `ConversationMap`).
- One `Conversation` may contain many `Task`s (one per `Turn`; see below).
- Different `Conversation`s are isolated by ACP session, but share the
  same underlying agent process and therefore share filesystem effects,
  API quotas, and rate limits.

**Continuity is best-effort, not guaranteed.** A `Conversation`'s memory
is held in the `Serve Shim`'s in-memory `ConversationMap` and the
associated ACP `sessionId` in the spawned `ACP Agent`. Continuity can be
lost by any of the following without notice to the `Caller`:

- `Serve Shim` restart (supervisor cycle, deploy, OOM)
- `ACP Agent` subprocess crash (which per spec section 2.4 also takes
  the `Serve Shim` with it)
- Idle sweep after `conversation_idle_secs` (default 24 h)

When continuity is lost, the next request carrying the same
`conversation_id` is treated as the first request of a brand-new
`Conversation`; the `ACP Agent` will have no memory of prior `Turn`s.
`Caller`s SHOULD treat continuity as an optimization, not a contract.

v1.1 will tighten this in two ways: (1) caller-side intent signaling
via a `conversation_mode` argument on `a2a_send`, and (2) persistence
plus ACP `session/resume` so cross-restart continuity becomes possible
when the `ACP Agent` supports it.

---

## Task

The protocol-level **A2A `Task` object** as defined by the Google A2A
specification: the unit returned from `message/send` and `message/stream`,
carrying an `id`, `status`, `history`, and `artifacts`.

**Lifetime.** A `Task` is short-lived. It is created when an inbound A2A
request arrives, transitions through `submitted → working → {completed |
failed | canceled}`, and is terminal as soon as the underlying ACP prompt
`Turn` ends. A `Task` is **not** revivable after reaching a terminal
state.

**Scope.** A `Task` represents exactly one `Turn` of work, not a
user-level goal. A user goal that needs N back-and-forth exchanges is N
`Task`s, typically grouped under a single `Conversation`.

**Anti-usage.** Do not say "long-running Task" — `Task`s are intrinsically
short. Say "long-running `Turn`" or "long `Conversation`" instead.

---

## Turn

One **request-and-response cycle** between a caller and the remote agent:
the caller sends a message, the agent processes it (possibly with internal
tool calls and intermediate updates), and produces a terminal result.

**Mappings.**

- 1 `Turn` ↔ 1 A2A `Task` ↔ 1 ACP `session/prompt` invocation.
- A `Turn`'s terminal outcome maps from ACP `stopReason` to A2A
  `TaskState` (`end_turn` → `completed`, `cancelled` → `canceled`,
  `refusal` / `max_tokens` / `max_turn_requests` → `failed`).

**Why a separate term.** "`Task`" is overloaded across A2A, ACP, and
everyday English. `Turn` names the business-level concept ("one
exchange") without protocol baggage, so phrases like "long-running Turn"
or "the second Turn of this Conversation" stay unambiguous.

---

## Caller

The party that **initiates** an A2A interaction. In the client-mode flow,
the `Caller` is whatever program drives `a2a_send`. In the serve-mode
flow, the `Caller` is the remote party that hits the A2A HTTP endpoint
(typically another shim instance in client mode, but may be any
A2A-conformant client).

Used when the sentence is about **who started the exchange**, regardless
of which side of the wire that party sits on.

---

## Host

The program that **spawns the client-mode shim** as a child process via
its MCP configuration (for example, Claude Code as a desktop app, an IDE
with MCP support, or a test harness). The `Host`'s lifecycle owns the
client-mode shim's lifecycle: when the `Host` exits, the shim's stdin
closes and the shim exits.

The `Host` is also the `Caller` in client-mode flows — but only one of
those words should appear in any given sentence, chosen by which aspect
matters: lifecycle/ownership uses `Host`; protocol direction uses
`Caller`.

`Host` is a **client-mode-only** term. Do not use it in serve-mode
writing.

---

## ACP Agent

The **child process spawned by serve mode** that implements the ACP
`Agent` role over stdio JSON-RPC. Examples: `claude-agent-acp`,
`codex-acp`, `gemini-cli` in ACP mode.

The serve-mode shim itself plays the **ACP `Client` role** opposite the
`ACP Agent` — this is the ACP protocol's terminology, not ours.

`ACP Agent` is a **serve-mode-only** term. Do not use it in client-mode
writing.

---

## LLM

The underlying language model API (Claude, GPT, a local model, etc.)
consumed by an `ACP Agent` or a `Host` to produce text. The shim is
**LLM-agnostic** and never talks to an `LLM` directly. Mention `LLM` only
when explaining behavior that originates from model decisions (e.g.,
"the `Host`'s `LLM` decides when to call `a2a_send`").

---

## "agent" (lowercase, informal)

When the word `agent` appears in lowercase and unqualified, it is **a
casual cover term** for "either a `Host` or an `ACP Agent`, doesn't
matter which". Permitted only in prose where the distinction genuinely
does not matter (e.g., "multi-agent discussion"). When the distinction
matters, use one of the precise terms above.

Capitalized `Agent` without the `ACP` prefix is **prohibited** because it
collides with the ACP `Agent` role in the official ACP documentation.

---

## Workspace

The **local filesystem, environment, and resources visible to a single
`ACP Agent` subprocess** — defined by the `cwd`, `env`, and host machine
on which serve mode is running. Each serve-mode deployment has exactly
one `Workspace`.

**Isolation property.** `Workspace`s are **not shared between the
`Caller` and the `ACP Agent`**. The `Caller`'s machine and the `ACP
Agent`'s machine are typically different (the port-forwarding layer
bridges them), and even when they happen to be the same machine, the
`cwd` is usually different. The `ACP Agent` cannot see the `Caller`'s
files, and vice versa.

**Implication for callers.** Any code, data, or context that the `ACP
Agent` needs MUST be passed inline in the `a2a_send` message. File paths
that mean something to the `Caller` are meaningless to the `ACP Agent`
unless they happen to coincide.

**Implication for prompts.** The system prompt of any `Host` configured
to use `a2a_send` SHOULD remind its `LLM` of this property — otherwise
the `LLM` will issue file-path-based requests to remote agents, which
then silently read unrelated files from their own `Workspace`.

---

## Client Shim / Serve Shim

The two operating modes of the `a2a-shim` binary. They are named after
their CLI subcommands but refer to running **instances**, not the binary
itself.

- **`Client Shim`** — a running instance launched as `a2a-shim client`.
  It is spawned by a `Host` via MCP configuration and acts as a stdio
  MCP server toward the `Host` and an outbound A2A HTTP client toward
  remote A2A endpoints. **It does not participate in ACP.**

- **`Serve Shim`** — a running instance launched as `a2a-shim serve`.
  It is an independently-supervised long-running process that accepts
  inbound A2A HTTP requests and spawns an `ACP Agent` subprocess. It
  participates in ACP as the `ACP Client role` (see below).

**Naming rules.**

- The bare word `shim` is **not permitted on its own** in design
  documents. Always say `Client Shim`, `Serve Shim`, or "the shim
  binary" (which refers to the compiled artifact that can run as
  either).
- Lowercase `client mode` / `serve mode` may be used for casual
  prose, but precise sentences MUST use the capitalized forms.

---

## ACP Client role

The **`Client` role defined by the ACP specification** — the side that
drives an `ACP Agent` by sending `initialize`, `session/new`,
`session/prompt`, and `session/cancel`, and that receives reverse
requests such as `session/request_permission`, `fs/*`, and `terminal/*`.

In our system, the **`Serve Shim` is the `ACP Client role`**, and the
spawned `ACP Agent` subprocess is the ACP Agent role. The `Client Shim`
is unrelated to this role.

**Naming rules.**

- When a sentence is about ACP-level behavior (capabilities, reverse
  RPC, session lifecycle), use `ACP Client role` or `ACP Agent role`,
  never bare `Client` or `Agent`.
- The capitalized bare word `Client` without the `ACP` prefix is
  **prohibited** because it collides with the `a2a-shim client`
  subcommand and with A2A HTTP clients.

---

## A2A HTTP client / A2A HTTP server

The two HTTP-level roles in an A2A interaction. Spelled out
explicitly when needed because every other "client" / "server" term
in this project is overloaded.

- The `Client Shim` contains an **A2A HTTP client** (outbound).
- The `Serve Shim` contains an **A2A HTTP server** (inbound).

No abbreviated form. Always write `A2A HTTP client` or `A2A HTTP
server` to keep these distinct from `ACP Client role` and from the
`Client Shim` / `Serve Shim` names.
