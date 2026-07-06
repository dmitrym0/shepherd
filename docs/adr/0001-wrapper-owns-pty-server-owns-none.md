# 0001 — Wrapper owns the PTY; Server owns none

## Status

Accepted (2026-07-06)

## Context

Shepherd extracts agent supervision from herdr. Herdr's architecture is a detached server that owns agent PTYs, with the TUI attaching over a wire protocol — that buys persistence (agents survive terminal close) at the cost of an attach/handoff protocol layer, the largest chunk of non-pure code in herdr.

Shepherd's goal is terminal-independent *monitoring*, and the user runs agents directly in their own terminal.

## Decision

Agents run under a **Wrapper** (`shepherd run <agent>`) — a transparent PTY shim in the user's terminal that spawns the agent, passes bytes through untouched, runs state detection, and reports to the Server. The **Server owns zero PTYs and zero terminals**: it only aggregates Wrapper reports and fans them out to Monitors.

## Consequences

- Agents do **not** survive terminal close. No attach/reattach, no wire protocol, no resize negotiation.
- The user's terminal stays fully native: rendering, scrollback, copy/paste untouched.
- Extraction from herdr shrinks to the pure modules (detect/, pane/agent_detection.rs, terminal/state.rs) plus one small PTY shim.
- The Server is trivially terminal-independent and could later accept reports from other sources (detached runtimes, remote machines) without redesign — the Wrapper→Server reporting channel is the stable seam.
- If persistence is ever needed, it can be added as a new kind of Wrapper host without changing the Server.
