# Research: Navigate to Agent Terminal

No NEEDS CLARIFICATION markers remained in the Technical Context; this documents the decisions behind the plan.

> **Revision (2026-09-10)**: D1, D5 and D6 are **superseded** — per user direction the feature ships as a standalone `tools/focus-agent.sh` script, not a server endpoint. D2 (osascript mechanism), D3 (tmux mechanism) and D4's failure taxonomy carry over into the script; see `contracts/tools-focus.md`.

## D1. ~~Where focus executes: server-side, single endpoint~~ (superseded)

**Decision**: `shep serve` executes focus via `POST /agents/{agent_id}/focus`. The web monitor and the `shep focus` CLI are both thin clients of that endpoint.

**Rationale**: The browser cannot run osascript, so a server endpoint is mandatory for the P1 story anyway. Routing the CLI through the same endpoint yields one code path, and concentrates the macOS Automation permission grant in one process (`shep serve`) instead of every terminal the user types `shep focus` in.

**Alternatives considered**: CLI executes locally (duplicate code path, N permission prompts); a separate helper binary (new artifact for no gain); browser-side custom URL scheme like `iterm2://` (no reliable focus-session scheme exists, and it wouldn't cover tmux).

## D2. iTerm2 focus mechanism: osascript session scan

**Decision**: Shell out to `/usr/bin/osascript` with the windows→tabs→sessions scan already published as the consumer example in docs/API.md: match `id of session` against the recorded UUID, then `select t`, `select s`, `activate`. The script returns a marker string on match so the caller can distinguish "focused" from "not found".

**Rationale**: The approach is already documented and known to work; the session UUID survives tab reordering (why the positional prefix was dropped at capture time). No crate needed.

**Alternatives considered**: iTerm2's Python API (requires user-enabled API server and a Python runtime — heavy); Accessibility APIs (much harder, more permissions); iTerm2 proprietary escape sequences (act on the *current* session, cannot target another).

## D3. tmux focus mechanism: switch-client + select-window + select-pane

**Decision**: For `terminal.app == "tmux"`, run `tmux switch-client -t <pane>`, `tmux select-window -t <pane>`, `tmux select-pane -t <pane>` (pane ids like `%5` are globally unique targets). If `tmux list-clients` shows no attached client, report "session not attached". Afterwards, best-effort raise the hosting terminal: if iTerm2 is running, `osascript -e 'tell application "iTerM2" to activate'`-style plain activate (no session targeting — shepherd doesn't know which terminal hosts the tmux client).

**Rationale**: Pane id is exactly what capture recorded; these three commands are the canonical way to make a pane the visible focus for the attached client. The hosting-terminal raise is best-effort because that identity was deliberately not captured (the inherited iTerm id can be stale — see the Terminal Location docs).

**Alternatives considered**: recording the hosting terminal at capture time (known-stale data, why it was excluded); only selecting the pane without switch-client (fails when the client is attached to a different session).

## D4. Failure taxonomy

**Decision**: The endpoint distinguishes and returns: `focused` (200), `not_found` — agent id unknown (404), `no_terminal` — agent has no Terminal Location (409), `terminal_gone` — scan/target found nothing (410), `not_attached` — tmux, no client (409), `focus_failed` — osascript/tmux errored, stderr included (502; covers the denied-Automation-permission case, with a hint to check System Settings → Privacy & Security → Automation).

**Rationale**: FR-005/FR-006 require honest, human-readable failures and never focusing the wrong window; each case maps to a distinct user message in monitor toast and CLI stderr.

**Alternatives considered**: boolean success (hides why; fails FR-005).

## D5. Amending the "monitors are read-only" contract

**Decision**: docs/API.md changes from "monitors are read-only" to "monitors are read-only except `POST /agents/{agent_id}/focus`", explicitly noting the endpoint acts only on the server's own machine (localhost bind) and never mutates agent state (FR-008).

**Rationale**: The read-only statement is a published consumer contract; silently violating it is worse than amending it. Focus does not change any agent object — `revision` does not increment — so the spirit (state is read-only) survives.

**Alternatives considered**: separate port/socket for actions (ceremony without benefit at localhost scale).

## D6. Web monitor affordance

**Decision**: A focus button on each agent card, rendered only when the agent object carries `terminal` (FR-006). Click → `fetch POST /agents/{id}/focus`; non-200 → transient toast with the returned message. No optimistic UI; last click wins (matches the spec's concurrency edge case).

**Rationale**: Single-file vanilla JS monitor already renders per-agent cards from the same JSON; a conditional button is a small delta.
