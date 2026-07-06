# Shepherd — Ubiquitous Language

## Terms

**Agent** — an AI coding agent process (Claude Code, Codex, etc.) spawned by and running under a Wrapper, in the user's own terminal.

**Wrapper** — the `shepherd run <agent>` process. A transparent PTY shim: spawns the Agent under a PTY in the user's terminal, passes bytes through untouched, runs state detection on the output stream, and reports to the Server. Owns the Agent's lifecycle; the Agent dies when the Wrapper (or its terminal) dies.

**Server** — the aggregation point. Owns no PTYs and no terminals. Receives state reports from Wrappers and fans them out to Monitors in realtime.

**Monitor** — any client connected to the Server to observe Agent status (web page, phone, CLI, bot).

**Agent State** — the *detected* status of an Agent: `Idle`, `Working`, `Blocked` (needs human input), `Unknown`. Inherited from herdr's model.

**Agent Status** — the *Monitor-facing* status: Agent State plus the derived `Done` state (five states total: `Idle`, `Working`, `Blocked`, `Done`, `Unknown`).

**Done** — derived status meaning "the Agent went Idle and the user hasn't been Seen since." Exists so a Monitor can answer "did something finish while I was away?"

**Seen** — evidence that the user is back at the Agent's terminal. Inferred by the Wrapper from local keyboard input after an Idle transition (typing into the agent proves you saw it). Not an API operation — Monitors cannot ack.

**Custom Status** — short free-text status self-reported by an Agent via Hook authority (e.g. "thinking…"). Max 32 chars, display-only.

**State Labels** — free-form key/value metadata self-reported via Hook authority (e.g. `tool: bash`). Display-only.

**Blocked Reason** — the message accompanying a `Blocked` state explaining what the Agent is waiting on (e.g. a permission prompt). Shipped as a string; Monitors never see terminal output.

**Agent Session** — a reference to the Agent's own resumable session (e.g. a Claude Code session id), self-reported via hooks. Carried as opaque metadata.

**Terminal Location** — the identity of the terminal the Agent's Wrapper runs in (e.g. an iTerm2 session UUID). Captured by the Wrapper from its environment at registration; immutable for the Agent's lifetime. Identity, not presentation: distinct from State Labels, which integrations may overwrite. Shepherd only publishes it — acting on it (revealing the tab) is a consumer concern.

**Detection** — inferring Agent State. Two pathways, also inherited from herdr:
- **Screen detection** — pattern-matching rule manifests against the Agent's recent terminal output. The fallback; works for any agent.
- **Hook authority** — the Agent self-reports state via installed hooks. Overrides screen detection when present.
