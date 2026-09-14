# Changelog

## v0.5.0 — 2026-09-14

### Added

- Releases should produce a changelog (fcbb37b)

### Fixed and changed

- Match the release annotation exactly, not as a substring
## v0.4.0 — 2026-09-14

### Added

- Surface Claude's idle-session summaries in shep interfaces (49385c0)

### Fixed and changed

- Session identity is lost on server restart, silently breaking durable metadata and resume (69681aa)
- Focusing an iTerm session sometimes raises the wrong window (right tab number, wrong window) (78f7d12)
- Select the iTerm window, not just the tab, when focusing
- Teach the shep-meta skill that pinning means pinned=true

## v0.3.0 — 2026-09-11

### Added

- First-class opencode support (hook integration, not just screen detection) (3a1d3fb)

### Fixed and changed

- Ignore locally installed agent skill artifacts
- Differentiate agents in the web UI with a per-agent badge

## v0.2.1 — 2026-09-10

### Fixed and changed

- shep never reports Claude agents as "working", so the Working→Idle transition (6d672ac)
- Exclude already-shipped tickets from release counting
- Sync claude.toml from herdr 2026.09.04.1; detect half-circle spinner
- Document the herdr sync review process
- Fix greedy assert_match rewrite in release script

## v0.2.0 — 2026-09-10

### Added

- Web UI: group related sessions by directory under one heading (b4c0e39)
- Session metadata: free-form key/values with well-known keys (jira, description) (0748bec)

### Fixed and changed

- When the shepherd server restarts, running wrapper sessions are orphaned: the wrapper connects to the ingest unix socket once at startup (connect_or_start_server in src/wrapper.rs) and never reconnects. After the server comes back, existing sessions no longer report and the agent disappears from the dashboard until the wrapper itself is restarted. (e4ccf4a)
- Release infrastructure: one-command version bump, tag, and tap update (91b08c9)
- Use install(1) in install-local; brew kegs strip the write bit
- Add Justfile with install-local
- Document session metadata in the README
- Add speckit scaffolding and navigate-to-session spec
- Add terminal-focus tools: focus-agent.sh, SwiftBar click-through, Raycast extension

## v0.1.0 — 2026-07-08

### Fixed and changed

- Rename the binary to shep
- Propagate Claude Code /rename into the agent name
- Publish Terminal Location on agents
- Document the status-consumer API
- Implement shepherd: wrapper, server, monitors, and Claude hook
- Document shepherd domain model and founding decisions

_Reconstructed from commit history; this release predates release annotations, so it may be incomplete._
