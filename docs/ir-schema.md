# Session IR v1

The agent-neutral representation every session converts through. It is what
`asm export` emits, what `asm import` consumes, and the only place where the
fidelity contract between two agents is written down.

Source of truth: `crates/asm-core/src/ir/v1.rs`.

## Contract

**Everything the IR models converts cleanly. Everything it does not model rides
in per-agent `extensions` bags**, so an A → B → A round trip can restore what
the middle agent had no concept of. Opaque provider payloads (signed or
encrypted reasoning) are summarized, never carried — they are provider-bound by
construction and a receiving agent could not use them anyway.

An importer may therefore assume: if a field is present in the IR, it means the
same thing regardless of which agent produced it.

## Document

```jsonc
{
  "ir_version": 1,
  "source": {
    "agent": "claude-code",              // "claude-code" | "opencode"
    "native_id": "4c93a826-…",           // id in the SOURCE agent
    "agent_version": "2.1.233",
    "exported_at": "2026-08-18T02:11:04Z",
    "exporter_version": "0.1.0"          // the asm that wrote this
  },
  "title": "Add DSL templates",
  "slug": "magical-ripple",
  "project_path": "${HOME}/projects/rust/k8x",
  "created": "2026-06-15T19:09:50.473Z",
  "updated": "2026-08-10T15:44:00.669Z",
  "model": "claude-fable-5",
  "usage": { "cost_usd": 57.84, "input_tokens": 179, "output_tokens": 72586,
             "cache_read_tokens": null, "cache_write_tokens": null },
  "messages": [ /* see below */ ],
  "extensions": { "claude-code": { /* source-only records */ } }
}
```

### Portable paths

Every path is a `PortablePath`: a string with the user's home directory
replaced by the literal token `${HOME}`. Paths outside home are kept absolute.
Archives and exported documents therefore survive being moved to another machine
with a different home directory. Resolution happens at import time.

### Timestamps

RFC 3339 with an explicit offset, always. Both agents' native encodings (Claude's
strings, OpenCode's epoch milliseconds) are normalized on the way in.

## Messages

```jsonc
{
  "role": "assistant",                  // "user" | "assistant" | "system"
  "timestamp": "2026-08-01T10:00:05Z",
  "source_id": "u1",                    // native record/message id, drives idempotent import
  "parts": [ /* see below */ ],
  "extensions": {}
}
```

`source_id` is what makes re-import idempotent: target ids are derived
deterministically from the source session id and these per-message ids, so
importing the same session twice produces the same target ids and is detected as
already in sync rather than duplicated.

## Parts

A tagged union on `type`. Unknown values deserialize to `unknown` rather than
failing, so a document written by a newer `asm` still loads.

| `type` | Fields | Notes |
|---|---|---|
| `text` | `text` | plain conversation content |
| `reasoning` | `summary`, `opaque` | `opaque: true` means the original was a signed/encrypted provider payload and only readable text survives |
| `tool_call` | `call_id`, `name`, `input` | `name` is the **source** agent's tool name; mapping happens at import |
| `tool_result` | `call_id`, `output`, `is_error`, `truncated` | paired to its call by `call_id` |
| `file` | `path`, `mime`, `content` | `path` is a `PortablePath` |
| `agent` | `name`, `description`, `transcript` | a nested subagent run, attached to the turn that spawned it |
| `unknown` | — | forward compatibility |

Tool calls and their results are separate parts even where an agent stores them
as one record, because the two agents disagree about that packaging: Claude
answers an assistant `tool_use` with a `tool_result` in the *next user record*,
while OpenCode keeps input and output in a single `tool` part. Splitting them in
the IR lets each importer repackage without guessing.

## Extensions

Keyed by agent name. Currently produced:

- `claude-code.state_records` — `ai-title`, `last-prompt`, `mode` and similar
  records that are session state rather than conversation.
- `claude-code.skipped_record_counts` — a census of record types the exporter
  did not model, so the loss report can name them instead of silently dropping
  them.
- `opencode.raw_parts` (per message) — `step-start`, `step-finish`, `patch`,
  `snapshot`, `compaction` and anything newer, preserved verbatim.

Importers must ignore extension keys they do not recognize.

## Versioning

`ir_version` is a single integer. A reader must refuse a document whose version
it does not know rather than guess. Additive changes (new optional fields, new
part types) do not bump it, because both are already tolerated by readers:
unknown fields are ignored and unknown part types deserialize to `unknown`. Any
change to the meaning of an existing field bumps the version.
