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

## Configuration

| Env var                | Default                     |
|------------------------|-----------------------------|
| `SHEPHERD_SOCKET_PATH` | `~/.shepherd/shepherd.sock` |
| `SHEPHERD_HTTP_PORT`   | `4650`                      |
