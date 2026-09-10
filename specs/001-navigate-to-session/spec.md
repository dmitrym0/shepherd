# Feature Specification: Navigate to Agent Terminal

**Feature Branch**: `001-navigate-to-session`

**Created**: 2026-09-10

**Status**: Draft

**Input**: User description: "i would like to be able to navigate directly to the claude session/iterm tab or window where the claude session was started"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Jump to a blocked agent from the web monitor (Priority: P1)

A user runs several agents in different iTerm2 tabs and windows. The web monitor shows one of them as **blocked** — waiting on input. The user clicks a "go to terminal" affordance on that agent's card, and the exact iTerm2 tab (and window) hosting that agent comes to the foreground with keyboard focus, ready for the user to type.

**Why this priority**: The blocked state is the product's killer feature; today the user still has to hunt through tabs by hand to answer the agent. Closing that last step delivers the core promise of the monitor.

**Independent Test**: Start two agents in two different iTerm2 tabs, open the web monitor, click "go to terminal" on the second agent — its tab is focused. This alone is a viable MVP.

**Acceptance Scenarios**:

1. **Given** an agent running in an iTerm2 tab that is not frontmost, **When** the user activates "go to terminal" on that agent in the web monitor, **Then** that tab's window is raised, the tab is selected, and keyboard focus lands in the agent's session.
2. **Given** an agent running in a split pane within an iTerm2 tab, **When** the user activates "go to terminal", **Then** the correct pane within the tab receives focus.
3. **Given** an agent whose terminal tab has been closed while the agent record still exists, **When** the user activates "go to terminal", **Then** the user sees a clear message that the terminal can no longer be found, and nothing else changes.

---

### User Story 2 - Jump to an agent from the command line (Priority: P2)

A user checking `shep status` in some other terminal sees an agent that needs attention and wants to get to it without touching the mouse. They issue a single command naming the agent, and the agent's terminal tab comes to the foreground.

**Why this priority**: Keyboard-centric users live in the terminal; the status table already names every agent, so a jump command completes the loop for them. Valuable, but the monitor click covers the primary "noticed while away" flow first.

**Independent Test**: With an agent running in one iTerm2 tab, run the jump command from another terminal naming that agent — the agent's tab is focused.

**Acceptance Scenarios**:

1. **Given** a running agent with a known name, **When** the user runs the jump command with that name, **Then** the agent's terminal tab is raised and focused.
2. **Given** a name that matches no agent, **When** the user runs the jump command, **Then** the command reports that no such agent exists and exits with a failure status.
3. **Given** two agents whose names share a prefix, **When** the user runs the jump command with an ambiguous prefix, **Then** the command lists the matching agents instead of guessing.

---

### User Story 3 - Jump to an agent running inside tmux (Priority: P3)

A user runs agents inside tmux panes (attached through iTerm2). Activating "go to terminal" for such an agent selects the correct tmux window and pane in the attached client, and brings the hosting terminal to the foreground.

**Why this priority**: Shepherd already records tmux pane identity for these agents; without pane selection, tmux users get dropped at the wrong pane. Smaller audience than plain iTerm2 tabs, hence P3.

**Independent Test**: Start an agent in a background tmux window, activate "go to terminal" — the tmux client switches to that window/pane and the hosting terminal is raised.

**Acceptance Scenarios**:

1. **Given** an agent in a tmux pane that is not the active pane, **When** the user activates "go to terminal", **Then** the tmux client's active window and pane become the agent's pane.
2. **Given** an agent in a tmux session with no attached client, **When** the user activates "go to terminal", **Then** the user is told the session is not attached anywhere, rather than a silent no-op.

---

### Edge Cases

- Agent's terminal tab or window was closed after the agent registered: the user gets a "terminal not found" message; no other window is focused by mistake.
- Agent runs in a terminal application the system cannot address (not iTerm2, not tmux): the navigation affordance is absent or explains that navigation is unavailable for this agent, rather than failing on click.
- The operating system requires the user's permission for one application to control another: on first use the user is prompted by the OS; if permission is denied, the feature reports that navigation is blocked by permissions and how to fix it.
- The monitor is open in a browser on a different machine than the one running the agents: navigation acts on the machine where the agents run; if that is impossible to honor, the user is told why.
- Two monitors trigger navigation at nearly the same time: last request wins; no error state.
- Agent has ended (done/exited) but its terminal still exists: navigation still works — the user may want to read the final output.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: Each agent that was started in a recognizable terminal context MUST expose enough identity for a consumer to locate its terminal (this identity is already recorded today for iTerm2 sessions and tmux panes).
- **FR-002**: The web monitor MUST offer a per-agent navigation affordance that, when activated, brings the agent's terminal tab/window to the foreground with keyboard focus on the agent's session.
- **FR-003**: A command-line entry point MUST accept an agent name and perform the same navigation.
- **FR-004**: For agents inside tmux, navigation MUST select the agent's tmux window and pane in the attached client in addition to raising the hosting terminal.
- **FR-005**: When the recorded terminal can no longer be found (closed tab, dead pane, detached session), the system MUST report a clear, human-readable failure and MUST NOT focus an unrelated window.
- **FR-006**: When an agent has no usable terminal identity, the monitor MUST NOT present a navigation affordance that silently does nothing (hide it or mark it unavailable).
- **FR-007**: Navigation MUST only ever act on the machine where the agent's terminal lives; requests that cannot honor this MUST be refused with an explanation.
- **FR-008**: Failure to navigate MUST never affect the agent itself — the agent keeps running and its status is unchanged.

### Key Entities

- **Agent**: A supervised session with a name and a status; already carries an immutable terminal identity captured at registration.
- **Terminal Location**: The recorded identity of where an agent runs — the terminal application plus a stable session/pane identifier; the input to any navigation action.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: From noticing a blocked agent in the monitor, a user reaches that agent's terminal with a single action in under 3 seconds.
- **SC-002**: Navigation lands on the correct tab, window, and pane in 100% of cases where the recorded terminal still exists — including split panes and reordered tabs.
- **SC-003**: When the terminal no longer exists, 100% of attempts produce an explanatory message and zero attempts focus the wrong window.
- **SC-004**: Users with 5+ concurrent agent tabs no longer manually search tabs to answer a blocked agent (task time drops from tab-hunting to one action).

## Assumptions

- Primary environment is macOS with iTerm2, matching what shepherd's terminal-identity capture supports today; tmux panes are the second supported context. Other terminals are out of scope for v1 beyond a graceful "unavailable" state.
- Both entry points (web monitor click, CLI command) are in scope; the monitor click is the MVP slice.
- The monitor is normally used on the same machine as the agents; cross-machine navigation is out of scope for v1.
- The existing published terminal identity (iTerm2 session UUID, tmux pane id) is sufficient to locate a session; no new capture at agent start is expected beyond what exists.
- Granting the OS-level automation permission is a one-time user action and acceptable friction.
