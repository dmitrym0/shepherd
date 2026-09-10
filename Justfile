default:
    @just --list

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
    cp target/release/shep "$target"
    echo "overwrote $target -> $("$target" --version)"
    if pgrep -f "shep serve" > /dev/null; then
        echo "note: a shepherd server from the old binary is still running; pkill -f 'shep serve' to cycle it"
    fi
