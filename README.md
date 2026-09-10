# shepherd

Run AI coding agents in your own terminal; monitor their status anywhere.

Shepherd is a transparent PTY shim plus a small aggregation server. `shep run
<agent>` runs the agent in your terminal exactly as if you had launched it
directly — native rendering, scrollback, copy/paste — while detecting its
state (idle / working / blocked / done) and streaming status changes to a
local server that any monitor can watch in realtime.

Detection rules and arbitration logic are copied from
[herdr](https://github.com/ogulcancelik/herdr) (AGPL-3.0-or-later); shepherd
is AGPL-3.0-or-later accordingly. See `docs/adr/` for the founding decisions
and `CONTEXT.md` for the domain glossary.

## Install

```sh
brew install dmitrym0/tap/shep     # or: cargo install --path .
```

The project is *shepherd*; the binary is `shep` (the `shepherd` name belongs
to GNU Shepherd in homebrew-core).

## Usage

```sh
# Run an agent under supervision (auto-starts the server on first use):
shep run claude
shep run --name refactor claude --model opus

# Watch your agents:
open http://localhost:4650        # live web monitor
shep status                   # one-shot table
shep status --watch           # live table

# Let Claude Code report its resumable session id (optional, once):
shep install-claude-hook
```

The killer state is **blocked** — an agent waiting on your input. **done**
means an agent finished while you were away; it flips back to **idle** the
moment you type into it again.

## Session metadata

Tag a session with key=value pairs and find it later:

```sh
shep meta refactor jira=PROJ-123 description="Fixing login timeout"
shep meta refactor            # show its metadata
shep meta refactor jira=      # remove a key
shep status PROJ-123          # filter: bare term or key=value
```

Run inside a supervised session, `shep meta` needs no agent name — the
environment identifies it. Metadata follows the agent's resumable session id,
not the process: restart the server or resume the session days later and the
tags reattach (stored in `~/.shepherd/metadata.json`, at most 64 keys per
session).

Three keys get special treatment. `description` appears on the web card, the
status table and the Raycast subtitle. `jira` and `url` render as links; for
Jira, set the base URL once:

```sh
printf 'jira_base_url = "https://mycorp.atlassian.net"\n' > ~/.shepherd/config.toml
```

Every list of sessions is searchable: the web monitor has a filter box,
`shep status <term>` filters the table and Raycast matches metadata in its
search bar.

Claude can tag its own session. `shep install-claude-hook` installs a
`shep-meta` skill beside the session hook; say "tag this session with jira
PROJ-123" and Claude runs `shep meta` itself.

## How it works

```
your terminal                             localhost
┌───────────────────────┐
│ shep run claude        │── screen detection ──┐
│  (PTY shim, bytes pass │                      ▼
│   through untouched)   │                 shep serve ──── ws://:4650/ws
│    └─ claude ──────────│── hook reports ──▶ (owns no PTYs)  GET /agents
└───────────────────────┘   unix socket                       GET /
```

- The wrapper feeds PTY output to a headless VT parser and matches the visible
  screen against per-agent rule manifests (`src/detect/manifests/*.toml`,
  18 agents supported), debounced to avoid state flaps.
- Agents' own hooks can report richer state over the same unix socket
  (`SHEPHERD_SOCKET_PATH` / `SHEPHERD_AGENT_ID` are injected into the agent's
  environment); hook reports take authority over screen detection.
- The server holds current state in memory only. Monitors get a snapshot on
  connect, then full-object deltas. Everything binds localhost.

## Building a monitor

Polling (`GET /agents`), streaming (`ws://localhost:4650/ws`), and the unix
socket all serve the same JSON — see [docs/API.md](docs/API.md) for the agent
object schema, status semantics, and the consumer contract.

## Tools

The `tools/` directory holds three ready-made consumers. Each can jump
straight to the iTerm2 tab or tmux pane where an agent runs.

| Tool                          | What it does |
|-------------------------------|--------------|
| `tools/focus-agent.sh`        | Focuses an agent's terminal, given a name, an iTerm2 session UUID or a tmux pane id. The other two build on it. |
| `tools/claude.5s.sh`          | SwiftBar menu-bar plugin: lists every agent, turns red when one needs input; clicking an agent focuses its terminal. |
| `tools/raycast-shepherd/`     | Raycast extension: a native, filterable list of agents with blocked ones on top; Enter focuses. Install with `npm install && npm run dev` in that directory. |

## Configuration

| Env var                | Default                     |
|------------------------|-----------------------------|
| `SHEPHERD_SOCKET_PATH` | `~/.shepherd/shepherd.sock` |
| `SHEPHERD_HTTP_PORT`   | `4650`                      |
