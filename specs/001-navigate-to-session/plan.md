# Implementation Plan: Navigate to Agent Terminal

**Branch**: `001-navigate-to-session` | **Date**: 2026-09-10 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/001-navigate-to-session/spec.md`

**Revision note (2026-09-10)**: The original plan proposed a `POST /agents/{id}/focus` server endpoint plus a `shep focus` subcommand. Per user direction, the delivery is instead a standalone executable script in a new `tools/` directory. No Rust changes, no new endpoint — shepherd's "revealing the tab is a consumer concern" stance stays intact; the script is the consumer.

## Summary

`tools/focus-agent.sh <name | iterm-uuid | tmux-pane-id>` resolves an agent's Terminal Location from `GET /agents` (or takes a session/pane id directly) and focuses it: osascript windows→tabs→sessions scan for iTerm2, `switch-client`/`select-window`/`select-pane` for tmux. Every failure mode is a distinct human-readable message on stderr with exit 1.

## Technical Context

**Language/Version**: Bash (macOS stock bash is fine; script is `#!/usr/bin/env bash`)

**Primary Dependencies**: `curl` + `jq` (name lookup only), `osascript` (iTerm2), `tmux` (tmux panes). No Rust changes, no crates.

**Storage**: N/A

**Testing**: `bash -n` syntax check + jq-logic smoke assertions; manual acceptance sweep per quickstart

**Target Platform**: macOS with iTerm2 and/or tmux

**Project Type**: Repo utility script (`tools/`)

**Performance Goals**: Sub-second focus at human window counts

**Constraints**: Runs on the machine with the terminals (FR-007 holds trivially). First iTerm2 use triggers the one-time macOS Automation prompt for the terminal the script runs from.

**Scale/Scope**: One new file: `tools/focus-agent.sh` (+ a README/API.md pointer)

## Constitution Check

`.specify/memory/constitution.md` is the unfilled template — no ratified gates; passes by default. Post-design re-check: passes (one script, zero dependencies added to the project).

## Project Structure

### Documentation (this feature)

```text
specs/001-navigate-to-session/
├── plan.md              # This file
├── research.md          # Phase 0 decisions (D1/D5/D6 superseded — see revision notes there)
├── data-model.md        # Phase 1 (entities unchanged; outcome table maps to script messages)
├── quickstart.md        # Phase 1
├── contracts/
│   └── tools-focus.md   # Script contract (replaces http-focus.md / cli-focus.md)
└── tasks.md             # Phase 2 (/speckit-tasks)
```

### Source Code (repository root)

```text
tools/
└── focus-agent.sh   # NEW: the whole feature

docs/
└── API.md           # optional: point the existing osascript example at tools/focus-agent.sh
```

**Structure Decision**: One executable script, no server or CLI surface. The web-monitor button (spec US1) and tmux host-terminal raise are reduced to what a script can do: US2 (command-line jump) becomes the P1 delivery; US1 is out of scope for this revision unless the user asks again — a browser cannot invoke a local script without a server-side action, which was explicitly declined.

## Complexity Tracking

No violations to justify.
