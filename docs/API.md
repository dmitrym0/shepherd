# Consuming agent status

Everything shepherd knows about your agents is available three ways. All of
them speak the same JSON shapes; pick by how you're connecting:

| You are…                          | Use                                        |
|-----------------------------------|--------------------------------------------|
| polling                           | `GET http://localhost:4650/agents`         |
| streaming (browser, bot, script)  | `ws://localhost:4650/ws`                   |
| local process, no HTTP stack      | the unix socket (`~/.shepherd/shepherd.sock`) |

The server binds localhost only. There is no auth and no history: you get
current state, updated in realtime.

## The agent object

Every API returns the same `AgentInfo` object. Absent optional fields are
omitted, not null.

```json
{
  "agent_id": "agent_3",
  "name": "refactor",
  "agent": "claude",
  "display_agent": "Claude Code",
  "title": "fixing the auth bug",
  "agent_status": "blocked",
  "custom_status": "waiting on permission",
  "state_labels": { "tab": "3" },
  "agent_session": {
    "source": "shepherd:claude",
    "agent": "claude",
    "kind": "session_id",
    "value": "01890a5d-...",
    "path": "/Users/you/.claude/projects/.../transcript.jsonl"
  },
  "blocked_reason": "Claude wants to run: rm -rf node_modules",
  "cwd": "/Users/you/project",
  "terminal": { "app": "iTerm2", "session_id": "1B0DF43A-DAA4-4C55-A299-4F0B6C3C1DAA" },
  "pid": 63477,
  "revision": 17,
  "status_since_ms": 1783377610632
}
```

| Field             | Type    | Meaning |
|-------------------|---------|---------|
| `agent_id`        | string  | Stable for the agent's lifetime; unique per server run. |
| `name`            | string? | Display name. Seeded by `shepherd run --name`; agents can overwrite it (Claude Code's `/rename` propagates here). Latest write wins. |
| `agent`           | string? | Canonical agent label (`claude`, `codex`, …); absent if unrecognized. |
| `display_agent`   | string? | Prettier agent name, hook-reported. |
| `title`           | string? | Free-text title, hook-reported. |
| `agent_status`    | enum    | `idle` \| `working` \| `blocked` \| `done` \| `unknown` — see below. |
| `custom_status`   | string? | Short self-reported status (≤32 chars), e.g. "thinking…". |
| `state_labels`    | object  | Free-form key/value metadata from integrations. |
| `agent_session`   | object? | The agent's own resumable session ref (e.g. for `claude --resume`). |
| `blocked_reason`  | string? | Why the agent is blocked. Only present when a hook reported it; screen-detected blocks carry no reason. |
| `cwd`             | string? | Working directory the agent was started in. |
| `terminal`        | object? | Terminal Location: which terminal the agent runs in — see below. |
| `pid`             | number  | Wrapper process id. |
| `revision`        | number  | Increments on every observable change to this object. |
| `status_since_ms` | number  | Epoch ms of the last `agent_status` change — render "blocked for 12m" from this. |

### Status semantics

- **`working`** — actively processing.
- **`blocked`** — waiting on human input. *This is the state worth alerting on.*
- **`done`** — finished while you were away: the agent went idle and the user
  hasn't typed into its terminal since. Flips to `idle` on the next local
  keystroke. There is no API to acknowledge it remotely — monitors are
  read-only.
- **`idle`** — finished, and the user has been at the terminal since.
- **`unknown`** — the wrapped command isn't a recognized agent, or its state
  can't be inferred.

### Terminal Location

`terminal` identifies the terminal the agent's wrapper runs in, captured from
the wrapper's environment at registration and immutable for the agent's
lifetime. Shepherd only publishes it — acting on it (revealing the tab) is
yours to implement.

| `app`    | `session_id`                       | Source |
|----------|------------------------------------|--------|
| `iTerm2` | the stable session UUID            | `ITERM_SESSION_ID` (positional `w0t4p0` prefix deliberately dropped — it goes stale on tab reorder; the UUID doesn't) |
| `tmux`   | the pane id, e.g. `%5`             | `TMUX_PANE` (reported instead of iTerm2 when inside tmux, where the inherited iTerm id may describe where the tmux *server* started, not your attached tab) |

Absent when the terminal isn't recognized. Example consumer — focus the
agent's iTerm2 tab:

```sh
SID=$(curl -s localhost:4650/agents | jq -r '.[0].terminal.session_id')
osascript -e "tell application \"iTerm2\"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if id of s is \"$SID\" then
          select t
          select s
          activate
          return
        end if
      end repeat
    end repeat
  end repeat
end tell"
```

## Polling: `GET /agents`

Returns a JSON array of agent objects, sorted by `agent_id`.

```sh
curl -s localhost:4650/agents | jq -r '.[] | "\(.agent_status)\t\(.name // .agent)"'
```

## Streaming: `ws://localhost:4650/ws`

On connect the server sends one snapshot, then one event per change. Every
event is a single JSON text frame:

```json
{ "event": "snapshot",      "data": { "agents": [ …AgentInfo… ] } }
{ "event": "agent_added",   "data": { …AgentInfo… } }
{ "event": "agent_updated", "data": { …AgentInfo… } }
{ "event": "agent_removed", "data": { "agent_id": "agent_3" } }
```

Consumer contract:

- **Deltas are full objects.** On `agent_added`/`agent_updated`, replace your
  entry for that `agent_id` wholesale; never merge fields.
- **A `snapshot` can arrive at any time**, not just on connect (the server
  resyncs slow consumers instead of dropping them). Treat it as "replace all
  state".
- Agent removal means the agent process (and its terminal) is gone — a
  wrapper crash and a normal exit look the same.
- Messages you send are ignored; the socket is read-only.

Minimal client:

```js
const agents = new Map();
const ws = new WebSocket("ws://localhost:4650/ws");
ws.onmessage = (m) => {
  const { event, data } = JSON.parse(m.data);
  if (event === "snapshot") { agents.clear(); data.agents.forEach(a => agents.set(a.agent_id, a)); }
  else if (event === "agent_removed") agents.delete(data.agent_id);
  else agents.set(data.agent_id, data);
};
```

## Unix socket

`~/.shepherd/shepherd.sock` (override: `SHEPHERD_SOCKET_PATH`). Protocol is
JSON lines: write `{"id": …, "method": …, "params": …}`, read one
`{"id", "result"|"error"}` line back.

Snapshot:

```sh
printf '{"id":1,"method":"agent.list","params":{}}\n' \
  | nc -U -w1 ~/.shepherd/shepherd.sock | jq .result.agents
```

Stream: send `{"id":1,"method":"events.subscribe","params":{}}`; after the
response, the connection becomes an event stream — the same event objects as
the WebSocket, one per line, starting with a snapshot. (`shepherd status
--watch` is exactly this.)

The socket also accepts the write side (registration, state and metadata
reports) — that's for wrappers and agent hooks, documented by the `Method`
enum in `src/protocol.rs`.
