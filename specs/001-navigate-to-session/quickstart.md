# Quickstart: Navigate to Agent Terminal

## Try it

```sh
# 1. Start an agent in an iTerm2 tab:
shep run --name demo claude

# 2. From any other terminal:
tools/focus-agent.sh demo      # the tab running 'demo' comes to the front

# No server needed if you already know the session:
tools/focus-agent.sh 1B0DF43A-DAA4-4C55-A299-4F0B6C3C1DAA   # iTerm2 session UUID
tools/focus-agent.sh %5                                     # tmux pane id
```

First iTerm2 focus triggers a one-time macOS prompt asking to allow control of iTerm2 — grant it to the terminal you ran the script from.

## tmux

Agents inside tmux get the correct window **and** pane selected in the attached client, then iTerm2 is raised best-effort. A detached session reports "not attached" instead of silently doing nothing.

## Verify

```sh
bash -n tools/focus-agent.sh   # syntax
```

Manual acceptance sweep (mirrors spec scenarios): two tabs → focus the background one; split pane → correct pane; close the tab → "no longer exists"; bogus name → error + `shep status` hint; two agents sharing a name → ambiguity error; tmux background window → client switches.
