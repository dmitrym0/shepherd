#!/usr/bin/env bash
# Focus the terminal tab/pane where a shepherd-supervised agent runs.
#
# Usage:
#   focus-agent.sh <agent-name>        # look up terminal via shep serve
#   focus-agent.sh <iterm-uuid>        # focus an iTerm2 session id directly
#   focus-agent.sh %5                  # focus a tmux pane id directly
#
# Requires: curl, jq (only for name lookup), osascript (iTerm2), tmux (tmux panes).
set -euo pipefail

PORT="${SHEPHERD_HTTP_PORT:-4650}"

die() { echo "focus-agent: $*" >&2; exit 1; }

[[ $# -eq 1 ]] || die "usage: focus-agent.sh <agent-name | iterm-session-uuid | tmux-pane-id>"
target="$1"

focus_iterm() {
  local sid="$1" found
  found=$(osascript <<EOF
tell application id "com.googlecode.iterm2"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if id of s is "$sid" then
          select t
          select s
          activate
          return "found"
        end if
      end repeat
    end repeat
  end repeat
end tell
return "gone"
EOF
  ) || die "osascript failed — check System Settings > Privacy & Security > Automation"
  [[ "$found" == "found" ]] || die "iTerm2 session $sid no longer exists"
}

focus_tmux() {
  local pane="$1"
  tmux has-session 2>/dev/null || die "no tmux server running"
  [[ -n "$(tmux list-clients 2>/dev/null)" ]] || die "tmux session is not attached anywhere"
  tmux switch-client -t "$pane" 2>/dev/null || true  # ponytail: fails when run from outside tmux; window/pane select below still lands it
  tmux select-window -t "$pane" && tmux select-pane -t "$pane" \
    || die "tmux pane $pane no longer exists"
  # Best effort: raise the terminal hosting the tmux client (host identity isn't recorded).
  osascript -e 'tell application id "com.googlecode.iterm2" to activate' 2>/dev/null || true
}

# Direct session ids skip the server lookup.
if [[ "$target" =~ ^%[0-9]+$ ]]; then
  focus_tmux "$target"; exit 0
elif [[ "$target" =~ ^[0-9A-Fa-f-]{36}$ ]]; then
  focus_iterm "$target"; exit 0
fi

# Otherwise treat it as an agent name and ask shep serve.
command -v jq >/dev/null || die "jq is required for name lookup"
agents=$(curl -sf "localhost:$PORT/agents") || die "shep serve not reachable on port $PORT"

matches=$(jq -c --arg n "$target" '[.[] | select(.name == $n or .agent == $n)]' <<<"$agents")
case "$(jq 'length' <<<"$matches")" in
  0) die "no agent named '$target' (run: shep status)" ;;
  1) ;;
  *) die "ambiguous name '$target': $(jq -r '[.[] | (.name // .agent)] | join(", ")' <<<"$matches")" ;;
esac

app=$(jq -r '.[0].terminal.app // empty' <<<"$matches")
sid=$(jq -r '.[0].terminal.session_id // empty' <<<"$matches")
[[ -n "$app" && -n "$sid" ]] || die "agent '$target' has no recorded terminal location"

case "$app" in
  iTerm2) focus_iterm "$sid" ;;
  tmux)   focus_tmux "$sid" ;;
  *)      die "unsupported terminal app: $app" ;;
esac
