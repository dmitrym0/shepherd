default:
    @just --list

# Cut a release; bump size derived from git-bug tickets closed since the last
# tag (feature -> minor, else patch). See docs/RELEASING.md.
release *ARGS:
    scripts/release.sh {{ARGS}}

# Manual release: `just release-manual 0.3.0` or `just release-manual minor`.
release-manual target *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{target}}" in
      major|minor|patch) exec scripts/release.sh --bump "{{target}}" {{ARGS}} ;;
      *) exec scripts/release.sh --version "{{target}}" {{ARGS}} ;;
    esac

# Build the release binary and overwrite the brew-installed shep with it.
install-local:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release
    prefix="$(brew --prefix shep 2>/dev/null || true)"
    target="$prefix/bin/shep"
    if [ -z "$prefix" ] || [ ! -x "$target" ]; then
        echo "brew-installed shep not found; install it first: brew install dmitrym0/tap/shep" >&2
        exit 1
    fi
    # brew installs binaries without the write bit; install(1) replaces in place
    install -m 0755 target/release/shep "$target"
    echo "overwrote $target -> $("$target" --version)"
    if pgrep -f "shep serve" > /dev/null; then
        echo "note: a shepherd server from the old binary is still running; pkill -f 'shep serve' to cycle it"
    fi
