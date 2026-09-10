# Contract: `tools/focus-agent.sh`

Standalone executable; focuses the terminal of a shepherd-supervised agent on the local machine.

## Invocation

```
tools/focus-agent.sh <agent-name>     # resolve terminal via GET /agents (needs curl + jq + shep serve)
tools/focus-agent.sh <iterm-uuid>     # 36-char UUID: focus that iTerm2 session directly, no server needed
tools/focus-agent.sh %5               # tmux pane id: focus that pane directly, no server needed
```

Name resolution: exact match on `name` or `agent` across `GET /agents`. Zero matches, ambiguous matches, and a missing `terminal` object are distinct errors — the script never guesses.

Port override: `SHEPHERD_HTTP_PORT` (default 4650), same as the server.

## Behavior

| Situation | stderr message (prefix `focus-agent:`) | Exit |
|-----------|----------------------------------------|------|
| Focused | — (silent success) | 0 |
| No/bad args | usage line | 1 |
| Server unreachable | `shep serve not reachable on port N` | 1 |
| No agent matches | `no agent named 'X' (run: shep status)` | 1 |
| Ambiguous name | `ambiguous name 'X': a, b` | 1 |
| Agent has no Terminal Location | `agent 'X' has no recorded terminal location` | 1 |
| iTerm2 session gone | `iTerm2 session <uuid> no longer exists` | 1 |
| Automation permission denied / osascript error | pointer to System Settings → Privacy & Security → Automation | 1 |
| No tmux server / pane gone / session detached | specific message each | 1 |

## Guarantees

- Never focuses a window other than the recorded target; every doubt is an error (FR-005).
- Reads agent state only; mutates nothing (FR-008).
- Local-machine only by construction (FR-007).
- tmux focus selects window **and** pane, then raises iTerm2 best-effort (host terminal identity is deliberately not recorded — see Terminal Location docs).
