//! State-transition debounce decisions for screen detection.
//!
//! Copied from herdr's pane/agent_detection.rs
//! (https://github.com/ogulcancelik/herdr), copyright Ogulcan Celik and
//! contributors, AGPL-3.0-or-later. Pure decision logic; no PTY involvement.

use crate::detect::{Agent, AgentDetection, AgentState};

pub const AGENT_PENDING_IDLE_RECHECK: std::time::Duration = std::time::Duration::from_millis(100);
const AGENT_PENDING_IDLE_CONFIRMATIONS: u8 = 3;
pub const AGENT_PENDING_IDLE_CAP: std::time::Duration = std::time::Duration::from_millis(700);
pub const STABLE_VISIBLE_SIGNAL_REFRESH: std::time::Duration =
    std::time::Duration::from_millis(800);
pub const AGENT_STARTUP_GRACE_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);
pub const DETECTION_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectionPublishState {
    pub state: AgentState,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
}

/// Holds a Working → plain-Idle transition for a few confirmations so
/// transient prompt frames don't flap the published state.
#[derive(Debug, Default)]
pub struct PendingIdleConfirmation {
    started_at: Option<std::time::Instant>,
    confirmations: u8,
}

impl PendingIdleConfirmation {
    pub fn active(&self) -> bool {
        self.started_at.is_some()
    }

    pub fn clear(&mut self) {
        self.started_at = None;
        self.confirmations = 0;
    }

    fn should_hold_working_to_idle(
        &mut self,
        previous: DetectionPublishState,
        next: DetectionPublishState,
        agent_changed: bool,
        process_exited: bool,
        now: std::time::Instant,
    ) -> bool {
        let is_working_to_plain_idle = previous.state == AgentState::Working
            && next.state == AgentState::Idle
            && !next.visible_idle
            && !next.visible_blocker
            && !agent_changed
            && !process_exited;

        if !is_working_to_plain_idle {
            self.clear();
            return false;
        }

        let Some(started_at) = self.started_at else {
            self.started_at = Some(now);
            self.confirmations = 0;
            return true;
        };

        if now.duration_since(started_at) >= AGENT_PENDING_IDLE_CAP {
            self.clear();
            return false;
        }

        self.confirmations = self.confirmations.saturating_add(1);
        if self.confirmations >= AGENT_PENDING_IDLE_CONFIRMATIONS {
            self.clear();
            return false;
        }

        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionScreenReadDecision {
    Read,
    Skip,
}

#[derive(Debug, Clone, Copy)]
pub struct DetectionScreenReadInput {
    pub state: AgentState,
    pub agent: Option<Agent>,
    pub pending_idle_active: bool,
    pub agent_changed: bool,
    pub process_exited: bool,
    pub current_detection_content_seq: Option<u64>,
    pub last_screen_scan_detection_content_seq: Option<u64>,
}

pub fn decide_detection_screen_read(
    input: DetectionScreenReadInput,
) -> DetectionScreenReadDecision {
    let skip = input.state == AgentState::Idle
        && input.agent.is_some()
        && !input.pending_idle_active
        && !input.agent_changed
        && !input.process_exited
        && input.current_detection_content_seq.is_some()
        && input.last_screen_scan_detection_content_seq == input.current_detection_content_seq;
    if skip {
        DetectionScreenReadDecision::Skip
    } else {
        DetectionScreenReadDecision::Read
    }
}

fn should_publish_detection_update(
    previous: DetectionPublishState,
    next: DetectionPublishState,
    agent_changed: bool,
    process_exited: bool,
    stable_visible_signal_refresh_due: bool,
) -> bool {
    next.state != previous.state
        || next.visible_idle != previous.visible_idle
        || next.visible_blocker != previous.visible_blocker
        || next.visible_working != previous.visible_working
        || agent_changed
        || process_exited
        || (stable_visible_signal_refresh_due && next.visible_blocker && previous.visible_blocker)
}

fn stable_visible_signal_refresh_due(
    previous: DetectionPublishState,
    next: DetectionPublishState,
    last_refresh: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    let stable_visible_signal = next.visible_blocker && previous.visible_blocker;

    stable_visible_signal
        && last_refresh.is_none_or(|last_refresh| {
            now.duration_since(last_refresh) >= STABLE_VISIBLE_SIGNAL_REFRESH
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionPublishDecision {
    NoPublish,
    Publish {
        state: AgentState,
        visible_idle: bool,
        visible_blocker: bool,
        visible_working: bool,
        process_exited: bool,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct ScreenDetectionPublishInput {
    pub current_state: AgentState,
    pub last_visible_idle: bool,
    pub last_visible_blocker: bool,
    pub last_visible_working: bool,
    pub last_visible_signal_refresh: Option<std::time::Instant>,
    pub screen_detection: AgentDetection,
    pub process_exited: bool,
    pub agent_changed: bool,
    pub now: std::time::Instant,
}

pub fn decide_screen_detection_publish(
    input: ScreenDetectionPublishInput,
    pending_idle: &mut PendingIdleConfirmation,
) -> DetectionPublishDecision {
    let detection = input.screen_detection;
    let new_state = detection.state;
    let visible_idle = detection.visible_idle && new_state == AgentState::Idle;
    let visible_blocker = detection.visible_blocker && new_state == AgentState::Blocked;
    let visible_working = detection.visible_working && new_state == AgentState::Working;

    let previous_publish = DetectionPublishState {
        state: input.current_state,
        visible_idle: input.last_visible_idle,
        visible_blocker: input.last_visible_blocker,
        visible_working: input.last_visible_working,
    };
    let next_publish = DetectionPublishState {
        state: new_state,
        visible_idle,
        visible_blocker,
        visible_working,
    };
    let stable_refresh_due = stable_visible_signal_refresh_due(
        previous_publish,
        next_publish,
        input.last_visible_signal_refresh,
        input.now,
    );

    if pending_idle.should_hold_working_to_idle(
        previous_publish,
        next_publish,
        input.agent_changed,
        input.process_exited,
        input.now,
    ) {
        return DetectionPublishDecision::NoPublish;
    }

    if should_publish_detection_update(
        previous_publish,
        next_publish,
        input.agent_changed,
        input.process_exited,
        stable_refresh_due,
    ) {
        return DetectionPublishDecision::Publish {
            state: new_state,
            visible_idle,
            visible_blocker,
            visible_working,
            process_exited: input.process_exited,
        };
    }

    DetectionPublishDecision::NoPublish
}

pub fn detection_update_for_publish_with_osc(
    agent: Option<Agent>,
    content: &str,
    osc_title: &str,
    osc_progress: &str,
    process_exited: bool,
) -> Option<AgentDetection> {
    if process_exited {
        return Some(AgentDetection {
            state: AgentState::Idle,
            skip_state_update: false,
            visible_idle: true,
            visible_blocker: false,
            visible_working: false,
        });
    }

    let detection = crate::detect::detect_agent_with_osc(agent, content, osc_title, osc_progress);
    (!detection.skip_state_update).then_some(detection)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publish_state(state: AgentState) -> DetectionPublishState {
        DetectionPublishState {
            state,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
        }
    }

    fn screen_detection(state: AgentState) -> AgentDetection {
        AgentDetection {
            state,
            skip_state_update: false,
            visible_idle: state == AgentState::Idle,
            visible_blocker: false,
            visible_working: state == AgentState::Working,
        }
    }

    fn screen_publish_input(
        current_state: AgentState,
        screen_detection: AgentDetection,
        now: std::time::Instant,
    ) -> ScreenDetectionPublishInput {
        ScreenDetectionPublishInput {
            current_state,
            last_visible_idle: false,
            last_visible_blocker: false,
            last_visible_working: false,
            last_visible_signal_refresh: None,
            screen_detection,
            process_exited: false,
            agent_changed: false,
            now,
        }
    }

    #[test]
    fn pending_idle_holds_working_to_plain_idle_until_confirmed() {
        let now = std::time::Instant::now();
        let previous = publish_state(AgentState::Working);
        let next = publish_state(AgentState::Idle);
        let mut pending = PendingIdleConfirmation::default();

        assert!(pending.should_hold_working_to_idle(previous, next, false, false, now));
        assert!(pending.should_hold_working_to_idle(
            previous,
            next,
            false,
            false,
            now + AGENT_PENDING_IDLE_RECHECK
        ));
        assert!(pending.should_hold_working_to_idle(
            previous,
            next,
            false,
            false,
            now + AGENT_PENDING_IDLE_RECHECK * 2
        ));
        assert!(!pending.should_hold_working_to_idle(
            previous,
            next,
            false,
            false,
            now + AGENT_PENDING_IDLE_RECHECK * 3
        ));
    }

    #[test]
    fn visible_idle_bypasses_plain_idle_hold() {
        let now = std::time::Instant::now();
        let previous = publish_state(AgentState::Working);
        let mut next = publish_state(AgentState::Idle);
        next.visible_idle = true;
        let mut pending = PendingIdleConfirmation::default();

        assert!(!pending.should_hold_working_to_idle(previous, next, false, false, now));
    }

    #[test]
    fn screen_read_skips_unchanged_idle_bottom_buffer() {
        let input = DetectionScreenReadInput {
            state: AgentState::Idle,
            agent: Some(Agent::Claude),
            pending_idle_active: false,
            agent_changed: false,
            process_exited: false,
            current_detection_content_seq: Some(10),
            last_screen_scan_detection_content_seq: Some(10),
        };
        assert_eq!(
            decide_detection_screen_read(input),
            DetectionScreenReadDecision::Skip
        );

        let changed = DetectionScreenReadInput {
            current_detection_content_seq: Some(11),
            ..input
        };
        assert_eq!(
            decide_detection_screen_read(changed),
            DetectionScreenReadDecision::Read
        );
    }

    #[test]
    fn screen_publish_publishes_working_transition() {
        let now = std::time::Instant::now();
        let mut pending_idle = PendingIdleConfirmation::default();

        assert_eq!(
            decide_screen_detection_publish(
                screen_publish_input(AgentState::Idle, screen_detection(AgentState::Working), now),
                &mut pending_idle,
            ),
            DetectionPublishDecision::Publish {
                state: AgentState::Working,
                visible_idle: false,
                visible_blocker: false,
                visible_working: true,
                process_exited: false,
            }
        );
    }

    #[test]
    fn screen_publish_dedupes_unchanged_state() {
        let now = std::time::Instant::now();
        let mut pending_idle = PendingIdleConfirmation::default();
        let mut input =
            screen_publish_input(AgentState::Working, screen_detection(AgentState::Working), now);
        input.last_visible_working = true;

        assert_eq!(
            decide_screen_detection_publish(input, &mut pending_idle),
            DetectionPublishDecision::NoPublish
        );
    }
}
