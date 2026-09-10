# Data Model: Navigate to Agent Terminal

No new persisted entities. The feature consumes one existing entity and introduces one transient value type.

## Existing (unchanged)

### Agent (`AgentInfo`, src/protocol.rs)

Unchanged. Relevant fields:

| Field | Type | Role here |
|-------|------|-----------|
| `agent_id` | string | Path parameter of the focus endpoint |
| `name` | string? | What `shep focus <name>` matches against (falls back to `agent`) |
| `terminal` | `TerminalLocation?` | Input to the focus action; absence disables the affordance |

Invariant preserved: focus never mutates an agent — `revision` does not increment (FR-008).

### TerminalLocation (src/protocol.rs)

Unchanged. `app` (`"iTerm2"` \| `"tmux"`) selects the focus strategy; `session_id` is the target (iTerm2 session UUID or tmux pane id).

## New (transient, in `src/focus.rs`)

### FocusOutcome

Result of one focus attempt; serialized in the endpoint response and mapped to CLI exit codes / monitor toasts.

| Variant | Meaning | HTTP |
|---------|---------|------|
| `focused` | Correct tab/window/pane now frontmost and focused | 200 |
| `no_terminal` | Agent exists but has no Terminal Location | 409 |
| `terminal_gone` | Recorded session/pane no longer exists | 410 |
| `not_attached` | tmux pane exists but no client is attached | 409 |
| `focus_failed` | osascript/tmux execution error; carries detail (includes Automation-permission denials) | 502 |

(`not_found` — unknown `agent_id` — is a plain 404 before any focus attempt.)

State transitions: none. A focus attempt reads state and touches the window system; it writes nothing.

## Validation rules

- Endpoint acts only on agents present in the current in-memory registry.
- `app` values other than `iTerm2`/`tmux` (future capture sources) → `no_terminal` semantics: never guess, never focus an unrelated window (FR-005).
- Name resolution for the CLI: exact match on `name`, else exact match on `agent`, else unique-prefix match on `name`; ambiguous prefix → list candidates, exit non-zero (spec US2, scenario 3).
