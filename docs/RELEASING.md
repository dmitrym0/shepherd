# Releasing shep

## Policy

Version bumps are tied to tracker events, not judgment calls:

| Event since the last tag | Bump |
|--------------------------|------|
| Any `feature`-labeled git-bug ticket closed and merged | minor |
| Only bug/unlabeled tickets closed | patch |
| Breaking the monitor contract (docs/API.md) | major — always explicit, never automatic |
| Nothing closed | no release (refused; `--force` overrides) |

## Cutting a release

```sh
just release --dry-run     # show tickets, version and steps; change nothing
just release               # do it (asks for confirmation)
just release-manual 0.3.0  # explicit version
just release-manual minor  # explicit bump size
```

`scripts/release.sh` performs, in order: bump `Cargo.toml`/`Cargo.lock`, commit,
tag `vX.Y.Z`, push main and the tag, sha256 the tag tarball, rewrite the
version-bearing lines of `Formula/shep.rb` in dmitrym0/homebrew-tap via the
GitHub API, verify the published checksum, and comment `released in vX.Y.Z` on
every shipped ticket.

Each step is probed before it runs, so a failed release resumes by re-running
the same command. It refuses to run on a dirty tree or off main — no overrides;
fix the cause.

Requirements: `git-bug`, `gh` (authenticated with push rights to the tap),
`curl`, `shasum` — the standard maintainer setup.

## Manual fallback

If the script is unavailable:

1. Edit `version` in `Cargo.toml`; run `cargo build`; commit.
2. `git tag vX.Y.Z && git push origin main vX.Y.Z`
3. `curl -sL https://github.com/dmitrym0/shepherd/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256`
4. In dmitrym0/homebrew-tap `Formula/shep.rb`, set the `url` tag, the `sha256`,
   and the `assert_match "shep X.Y.Z"` line; commit and push.
