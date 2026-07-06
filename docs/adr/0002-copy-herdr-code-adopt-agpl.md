# 0002 — Copy herdr's supervision code; shepherd adopts AGPL-3.0

## Status

Accepted (2026-07-06)

## Context

The valuable supervision pieces in herdr — the ~20 per-agent detection manifests, the state-flap debounce, and the hook-vs-screen arbitration — encode trial-and-error that would have to be redone agent-by-agent in a clean-room reimplementation. Herdr is AGPL-3.0-or-later (dual-licensed commercially) and is a third-party project (ogulcancelik/herdr), so copying makes shepherd a derivative work.

## Decision

Copy herdr's pure supervision modules (detect/ incl. manifests, pane/agent_detection.rs, terminal/state.rs, event/schema types, integration hook assets as needed) into shepherd. Shepherd is licensed **AGPL-3.0-or-later**, retains herdr copyright attribution, and is written in **Rust** (the copied code decides the language).

## Consequences

- Full fidelity to herdr's status behavior (all 5 statuses, custom status, state labels, blocked reason, session refs) without re-deriving detection rules.
- As a personal single-machine tool, AGPL imposes no active obligations (no distribution, no remote users).
- Shepherd can never be closed-sourced or sold under a proprietary license without either a clean-room rewrite of the copied parts or a commercial license from herdr's author. This door is deliberately closed.
- Upstream herdr improvements to manifests can be re-copied at will.
