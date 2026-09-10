# Keeping up with herdr

Shepherd vendors its agent-detection rules — and the arbitration logic behind
them — from [herdr](https://github.com/ogulcancelik/herdr). Vendored code does
not update itself: when an agent changes its UI and upstream fixes detection,
we stay broken until someone syncs.

This has bitten us. Claude Code 2.1.228 (Aug 2026) changed its busy-spinner
glyphs; herdr fixed detection within days; our copy stayed frozen for a month
and shepherd silently stopped reporting **working** — which killed the derived
**done** state, the product's core promise (git-bug 6d672ac).

## What is vendored, and where the version lives

| What | Where | Version marker |
|------|-------|----------------|
| Per-agent detection manifests (18 agents) | `src/detect/manifests/*.toml` | `version` field at the top of each file (upstream release, e.g. `2026.06.10.1`) |
| Arbitration / metadata semantics | `src/state.rs` (header comment) | none — diff by hand when upstream's terminal/state.rs changes |
| Socket API shape | `src/protocol.rs` (header comment) | none — stable; changes are rare and deliberate |

Manifests are `include_str!`-compiled: a sync requires rebuild, reinstall
(`just install-local` or a release), and restarting wrapped sessions.

## Review cadence

Check for drift **monthly**, and additionally **whenever an agent ships a
notable update** (a new Claude Code major/minor is the usual trigger) or
**whenever a state looks wrong** (an agent that never shows working/blocked is
drift until proven otherwise).

## The check (five minutes)

```sh
# Our pinned versions:
grep -m1 version src/detect/manifests/*.toml

# Upstream's current ones (same layout as ours: src/detect/manifests/):
gh api repos/ogulcancelik/herdr/contents/src/detect/manifests/claude.toml \
  --jq '.content' | base64 --decode | grep -m1 version
# Start with claude.toml — it breaks most often and matters most.
```

If upstream is ahead for an agent we support:

1. File a git-bug ticket (label `p1` if Claude working/blocked detection is
   affected — that is the killer feature).
2. Sync the manifest(s) verbatim from upstream; keep the upstream `version`
   field intact — it is the staleness marker for next time.
3. `cargo test` — the bundled-manifest compile test proves the vendored engine
   accepts the new rules (`min_engine_version` guards the rest). A manifest
   the engine rejects stays at its old version; never ship a silently
   non-compiling rule set.
4. Verify live with a real session of the affected agent, then release
   (patch bump — see docs/RELEASING.md).

## Rules of thumb

- Sync whole files, not cherry-picked lines — partial syncs make the version
  field a lie.
- Claude first, the rest opportunistically (FR-007 of the 005 spec: never let
  a broken minor-agent manifest block a Claude fix).
- If this document's procedure and reality disagree, fix the document in the
  same commit that proves it wrong.
