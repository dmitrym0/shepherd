//! Server-owned per-agent state: arbitration between screen detection and
//! hook authority, metadata presentation, and the Seen/Done derivation.
//!
//! Arbitration and metadata semantics copied from herdr's terminal/state.rs
//! and terminal/metadata.rs (https://github.com/ogulcancelik/herdr),
//! copyright Ogulcan Celik and contributors, AGPL-3.0-or-later. Trimmed to
//! shepherd's wrapper model: one agent per entry, no respawn, no release
//! suppression, no pane coupling.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::detect::{agent_label, parse_agent_label, Agent, AgentState};
use crate::protocol::{
    now_epoch_ms, AgentInfo, AgentReportAgentParams, AgentReportMetadataParams,
    AgentReportSessionParams, AgentSessionInfo, AgentStatus, TerminalLocation,
};
use crate::store::{
    MetaValue, SessionMetadata, MAX_KEYS_PER_SESSION, MAX_KEY_CHARS, MAX_VALUE_CHARS,
};

const MAX_CUSTOM_STATUS_CHARS: usize = 32;
const MAX_MESSAGE_CHARS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    pub kind: String,
    pub value: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookAuthority {
    pub source: String,
    pub agent_label: String,
    pub state: AgentState,
    pub message: Option<String>,
    pub custom_status: Option<String>,
    pub reported_at: Instant,
    pub session_ref: Option<SessionRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedSession {
    pub source: String,
    pub agent: String,
    pub session_ref: SessionRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetadataEntry {
    title: Option<String>,
    display_agent: Option<String>,
    custom_status: Option<String>,
    state_labels: HashMap<String, String>,
    title_reported_at: Option<Instant>,
    display_agent_reported_at: Option<Instant>,
    custom_status_reported_at: Option<Instant>,
    state_label_reported_at: HashMap<String, Instant>,
    reported_at: Instant,
    ttl: Option<Duration>,
}

impl MetadataEntry {
    fn empty(now: Instant) -> Self {
        Self {
            title: None,
            display_agent: None,
            custom_status: None,
            state_labels: HashMap::new(),
            title_reported_at: None,
            display_agent_reported_at: None,
            custom_status_reported_at: None,
            state_label_reported_at: HashMap::new(),
            reported_at: now,
            ttl: None,
        }
    }

    fn is_expired(&self, now: Instant) -> bool {
        self.ttl
            .is_some_and(|ttl| now.duration_since(self.reported_at) >= ttl)
    }

    fn is_valid(&self, now: Instant) -> bool {
        if self.title.is_none()
            && self.display_agent.is_none()
            && self.custom_status.is_none()
            && self.state_labels.is_empty()
        {
            return false;
        }
        !self.is_expired(now)
    }
}

/// One supervised agent, as the server sees it.
pub struct AgentEntry {
    pub id: u64,
    pub name: Option<String>,
    pub cwd: String,
    pub pid: u32,
    terminal: Option<TerminalLocation>,
    spawned_agent_label: Option<String>,
    detected_agent: Option<Agent>,
    fallback_state: AgentState,
    fallback_visible_blocker: bool,
    fallback_observed_at: Option<Instant>,
    hook_authority: Option<HookAuthority>,
    metadata: HashMap<String, MetadataEntry>,
    user_metadata: SessionMetadata,
    persisted_session: Option<PersistedSession>,
    hook_report_sequences: HashMap<String, u64>,
    session_report_sequences: HashMap<String, u64>,
    metadata_report_sequences: HashMap<String, u64>,
    state: AgentState,
    seen: bool,
    revision: u64,
    status_since_ms: u64,
    last_info: Option<AgentInfo>,
}

impl AgentEntry {
    pub fn new(
        id: u64,
        name: Option<String>,
        agent: Option<String>,
        cwd: String,
        pid: u32,
        terminal: Option<TerminalLocation>,
    ) -> Self {
        let spawned_agent_label = agent
            .as_deref()
            .and_then(parse_agent_label)
            .map(|agent| agent_label(agent).to_string())
            .or(agent);
        Self {
            id,
            name,
            cwd,
            pid,
            terminal,
            detected_agent: spawned_agent_label.as_deref().and_then(parse_agent_label),
            spawned_agent_label,
            // A freshly registered agent was just launched by the user, who
            // is by definition at the terminal: Idle and Seen.
            fallback_state: AgentState::Idle,
            fallback_visible_blocker: false,
            fallback_observed_at: None,
            hook_authority: None,
            metadata: HashMap::new(),
            user_metadata: SessionMetadata::new(),
            persisted_session: None,
            hook_report_sequences: HashMap::new(),
            session_report_sequences: HashMap::new(),
            metadata_report_sequences: HashMap::new(),
            state: AgentState::Idle,
            seen: true,
            revision: 0,
            status_since_ms: now_epoch_ms(),
            last_info: None,
        }
    }

    // -- ingest ------------------------------------------------------------

    pub fn set_detected(
        &mut self,
        agent: Option<Agent>,
        state: AgentState,
        visible_blocker: bool,
        process_exited: bool,
        now: Instant,
    ) {
        if self.live_full_lifecycle_hook_authority() && !process_exited {
            // A live full-lifecycle hook owns state; screen detection only
            // refreshes the detected agent when it agrees with the hook.
            if self
                .hook_authority
                .as_ref()
                .and_then(|authority| parse_agent_label(&authority.agent_label))
                == agent
            {
                self.detected_agent = agent;
            }
            self.recompute_effective_state();
            return;
        }

        let previous_detected_agent = self.detected_agent;
        self.detected_agent = agent;
        self.fallback_state = state;
        self.fallback_visible_blocker = visible_blocker && state == AgentState::Blocked;
        self.fallback_observed_at = Some(now);

        let hook_matches_agent = self
            .hook_authority
            .as_ref()
            .is_some_and(|authority| parse_agent_label(&authority.agent_label) == agent);
        if process_exited && hook_matches_agent {
            let cleared_source = self
                .hook_authority
                .as_ref()
                .map(|authority| authority.source.clone());
            if let Some(source) = cleared_source {
                self.hook_report_sequences.remove(&source);
            }
            self.hook_authority = None;
        }
        if process_exited
            && self
                .persisted_session
                .as_ref()
                .is_some_and(|session| parse_agent_label(&session.agent) == agent)
        {
            self.persisted_session = None;
        }

        // The foreground agent changed out from under a hook authority that
        // belonged to the previous agent: demote the authority to a
        // persisted session so its resume ref survives.
        let hook_belongs_to_previous = previous_detected_agent.is_some()
            && agent != previous_detected_agent
            && self.hook_authority.as_ref().is_some_and(|authority| {
                parse_agent_label(&authority.agent_label) == previous_detected_agent
            });
        let hook_conflicts = self.hook_authority.as_ref().is_some_and(|authority| {
            agent.is_some() && parse_agent_label(&authority.agent_label) != agent
        });
        if hook_belongs_to_previous || hook_conflicts {
            self.persisted_session = self.hook_authority.as_ref().and_then(|authority| {
                authority
                    .session_ref
                    .as_ref()
                    .map(|session_ref| PersistedSession {
                        source: authority.source.clone(),
                        agent: authority.agent_label.clone(),
                        session_ref: session_ref.clone(),
                    })
            });
            self.hook_authority = None;
        }

        self.recompute_effective_state();
    }

    pub fn set_hook_authority(&mut self, params: AgentReportAgentParams, now: Instant) {
        if !accept_seq(&mut self.hook_report_sequences, &params.source, params.seq) {
            return;
        }
        let Some(state) = AgentState::parse(&params.state) else {
            return;
        };
        let agent_label = normalize_agent_label(&params.agent);
        let session_ref = params
            .agent_session_id
            .map(|value| SessionRef {
                kind: "session_id".to_string(),
                value,
                path: params.agent_session_path,
            })
            .or_else(|| {
                self.hook_authority
                    .as_ref()
                    .filter(|authority| authority.source == params.source)
                    .and_then(|authority| authority.session_ref.clone())
            });
        self.hook_authority = Some(HookAuthority {
            source: params.source,
            agent_label,
            state,
            message: normalize_text(params.message, MAX_MESSAGE_CHARS),
            custom_status: normalize_text(params.custom_status, MAX_CUSTOM_STATUS_CHARS),
            reported_at: now,
            session_ref,
        });
        self.recompute_effective_state();
    }

    pub fn set_session(&mut self, params: AgentReportSessionParams) {
        if !accept_seq(
            &mut self.session_report_sequences,
            &params.source,
            params.seq,
        ) {
            return;
        }
        let Some(value) = params.agent_session_id else {
            return;
        };
        let session_ref = SessionRef {
            kind: "session_id".to_string(),
            value,
            path: params.agent_session_path,
        };
        if let Some(authority) = self
            .hook_authority
            .as_mut()
            .filter(|authority| authority.source == params.source)
        {
            authority.session_ref = Some(session_ref.clone());
        }
        self.persisted_session = Some(PersistedSession {
            source: params.source,
            agent: normalize_agent_label(&params.agent),
            session_ref,
        });
    }

    pub fn set_metadata(&mut self, params: AgentReportMetadataParams, now: Instant) {
        if !accept_seq(
            &mut self.metadata_report_sequences,
            &params.source,
            params.seq,
        ) {
            return;
        }
        let entry = self
            .metadata
            .entry(params.source.clone())
            .or_insert_with(|| MetadataEntry::empty(now));
        if entry.is_expired(now) {
            *entry = MetadataEntry::empty(now);
        }
        if params.clear_title {
            entry.title = None;
            entry.title_reported_at = None;
        }
        if params.clear_display_agent {
            entry.display_agent = None;
            entry.display_agent_reported_at = None;
        }
        if params.clear_custom_status {
            entry.custom_status = None;
            entry.custom_status_reported_at = None;
        }
        if params.clear_state_labels {
            entry.state_labels.clear();
            entry.state_label_reported_at.clear();
        }
        let has_set_fields = params.title.is_some()
            || params.display_agent.is_some()
            || params.custom_status.is_some()
            || !params.state_labels.is_empty();
        if let Some(title) = normalize_text(params.title, MAX_MESSAGE_CHARS) {
            entry.title = Some(title);
            entry.title_reported_at = Some(now);
        }
        if let Some(display_agent) = normalize_text(params.display_agent, MAX_MESSAGE_CHARS) {
            entry.display_agent = Some(display_agent);
            entry.display_agent_reported_at = Some(now);
        }
        if let Some(custom_status) = normalize_text(params.custom_status, MAX_CUSTOM_STATUS_CHARS)
        {
            entry.custom_status = Some(custom_status);
            entry.custom_status_reported_at = Some(now);
        }
        for (state, label) in params.state_labels {
            entry.state_labels.insert(state.clone(), label);
            entry.state_label_reported_at.insert(state, now);
        }
        if has_set_fields || params.ttl_ms.is_some() {
            entry.reported_at = now;
            entry.ttl = params.ttl_ms.map(Duration::from_millis);
        }
    }

    pub fn clear_authority(&mut self, source: &str, seq: Option<u64>) {
        if !accept_seq(&mut self.hook_report_sequences, source, seq) {
            return;
        }
        if let Some(authority) = self
            .hook_authority
            .as_ref()
            .filter(|authority| authority.source == source)
        {
            if let Some(session_ref) = &authority.session_ref {
                self.persisted_session = Some(PersistedSession {
                    source: authority.source.clone(),
                    agent: authority.agent_label.clone(),
                    session_ref: session_ref.clone(),
                });
            }
            self.hook_authority = None;
            self.recompute_effective_state();
        }
    }

    pub fn mark_seen(&mut self) {
        self.seen = true;
    }

    /// Apply a `shep meta` write: validate everything first (all-or-nothing),
    /// then upsert/remove. An empty value removes the key. Returns the keys
    /// removed by this write so the caller can make the deletions durable.
    pub fn set_user_metadata(
        &mut self,
        entries: HashMap<String, String>,
    ) -> Result<Vec<String>, String> {
        if entries.is_empty() {
            return Err("entries must not be empty".to_string());
        }
        let mut upserts: Vec<(String, String)> = Vec::new();
        let mut removals: Vec<String> = Vec::new();
        for (key, raw_value) in entries {
            let key = key.trim().to_string();
            if key.is_empty()
                || key.chars().any(char::is_control)
                || key.chars().count() > MAX_KEY_CHARS
            {
                return Err(format!("invalid metadata key: {key:?}"));
            }
            let value: String = raw_value.chars().filter(|ch| !ch.is_control()).collect();
            let value = value.trim().to_string();
            if value.chars().count() > MAX_VALUE_CHARS {
                return Err(format!(
                    "value for {key:?} exceeds {MAX_VALUE_CHARS} characters"
                ));
            }
            if value.is_empty() {
                removals.push(key);
            } else {
                upserts.push((key, value));
            }
        }
        let new_keys = upserts
            .iter()
            .filter(|(key, _)| !self.user_metadata.contains_key(key))
            .count();
        let removed_existing = removals
            .iter()
            .filter(|key| self.user_metadata.contains_key(*key))
            .count();
        if self.user_metadata.len() - removed_existing + new_keys > MAX_KEYS_PER_SESSION {
            return Err(format!(
                "a session holds at most {MAX_KEYS_PER_SESSION} metadata keys"
            ));
        }
        let now = now_epoch_ms();
        for key in &removals {
            self.user_metadata.remove(key);
        }
        for (key, value) in upserts {
            self.user_metadata.insert(key, MetaValue { value, ts_ms: now });
        }
        Ok(removals)
    }

    pub fn user_metadata(&self) -> &SessionMetadata {
        &self.user_metadata
    }

    pub fn replace_user_metadata(&mut self, entries: SessionMetadata) {
        self.user_metadata = entries;
    }

    /// Durable-store key for this agent's resumable session, if one is known.
    pub fn session_store_key(&self) -> Option<String> {
        self.effective_session()
            .map(|session| format!("{}:{}", session.kind, session.value))
    }

    /// Latest write wins: --name seeds it, agent renames overwrite it.
    pub fn set_name(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() {
            self.name = Some(name.to_string());
        }
    }

    // -- arbitration ---------------------------------------------------------

    fn recompute_effective_state(&mut self) {
        let previous_state = self.state;
        let state = if self.visible_blocker_overrides_hook() {
            AgentState::Blocked
        } else {
            self.hook_authority
                .as_ref()
                .map(|authority| authority.state)
                .unwrap_or(self.fallback_state)
        };
        if state == AgentState::Idle
            && matches!(previous_state, AgentState::Working | AgentState::Blocked)
        {
            self.seen = false;
        }
        self.state = state;
    }

    fn visible_blocker_overrides_hook(&self) -> bool {
        if self.live_full_lifecycle_hook_authority() {
            return false;
        }
        self.fallback_visible_blocker
            && self.fallback_not_older_than_hook()
            && self.hook_authority.as_ref().is_some_and(|authority| {
                authority.state != AgentState::Blocked
                    && parse_agent_label(&authority.agent_label) == self.detected_agent
            })
    }

    fn fallback_not_older_than_hook(&self) -> bool {
        match (&self.fallback_observed_at, &self.hook_authority) {
            (Some(observed_at), Some(authority)) => *observed_at >= authority.reported_at,
            _ => false,
        }
    }

    fn live_full_lifecycle_hook_authority(&self) -> bool {
        self.hook_authority.as_ref().is_some_and(|authority| {
            crate::detect::full_lifecycle_hook_authority(&authority.source, &authority.agent_label)
        })
    }

    // -- presentation --------------------------------------------------------

    pub fn effective_agent_label(&self) -> Option<String> {
        self.hook_authority
            .as_ref()
            .map(|authority| authority.agent_label.clone())
            .or_else(|| self.detected_agent.map(|agent| agent_label(agent).to_string()))
            .or_else(|| self.spawned_agent_label.clone())
    }

    fn agent_status(&self) -> AgentStatus {
        match (self.state, self.seen) {
            (AgentState::Idle, false) => AgentStatus::Done,
            (AgentState::Idle, true) => AgentStatus::Idle,
            (AgentState::Working, _) => AgentStatus::Working,
            (AgentState::Blocked, _) => AgentStatus::Blocked,
            (AgentState::Unknown, _) => AgentStatus::Unknown,
        }
    }

    fn valid_metadata(&self, now: Instant) -> impl Iterator<Item = &MetadataEntry> {
        self.metadata
            .values()
            .filter(move |entry| entry.is_valid(now))
    }

    fn effective_custom_status(&self, now: Instant) -> Option<String> {
        let newest = self
            .valid_metadata(now)
            .filter(|entry| entry.custom_status.is_some())
            .max_by_key(|entry| entry.custom_status_reported_at)
            .and_then(|entry| entry.custom_status.clone());
        if newest.is_some() {
            return newest;
        }
        if self.visible_blocker_overrides_hook() {
            return None;
        }
        self.hook_authority
            .as_ref()
            .and_then(|authority| authority.custom_status.clone())
    }

    fn effective_session(&self) -> Option<AgentSessionInfo> {
        if let Some(authority) = &self.hook_authority {
            if let Some(session_ref) = &authority.session_ref {
                return Some(AgentSessionInfo {
                    source: authority.source.clone(),
                    agent: authority.agent_label.clone(),
                    kind: session_ref.kind.clone(),
                    value: session_ref.value.clone(),
                    path: session_ref.path.clone(),
                });
            }
        }
        self.persisted_session.as_ref().map(|session| AgentSessionInfo {
            source: session.source.clone(),
            agent: session.agent.clone(),
            kind: session.session_ref.kind.clone(),
            value: session.session_ref.value.clone(),
            path: session.session_ref.path.clone(),
        })
    }

    fn blocked_reason(&self) -> Option<String> {
        if self.state != AgentState::Blocked {
            return None;
        }
        self.hook_authority
            .as_ref()
            .filter(|authority| authority.state == AgentState::Blocked)
            .and_then(|authority| authority.message.clone())
    }

    /// True when any metadata entry carries a TTL, i.e. presentation can
    /// change without a new report arriving.
    pub fn has_ttl_metadata(&self) -> bool {
        self.metadata.values().any(|entry| entry.ttl.is_some())
    }

    /// Recompute the monitor-facing snapshot. Bumps the revision and the
    /// status_since timestamp when the snapshot changed since the last call.
    /// Returns the snapshot and whether it changed.
    pub fn snapshot(&mut self) -> (AgentInfo, bool) {
        let now = Instant::now();
        let status = self.agent_status();
        let mut info = AgentInfo {
            agent_id: format!("agent_{}", self.id),
            name: self.name.clone(),
            agent: self.effective_agent_label(),
            display_agent: self
                .valid_metadata(now)
                .filter(|entry| entry.display_agent.is_some())
                .max_by_key(|entry| entry.display_agent_reported_at)
                .and_then(|entry| entry.display_agent.clone()),
            title: self
                .valid_metadata(now)
                .filter(|entry| entry.title.is_some())
                .max_by_key(|entry| entry.title_reported_at)
                .and_then(|entry| entry.title.clone()),
            agent_status: status,
            custom_status: self.effective_custom_status(now),
            state_labels: {
                let mut labels: Vec<_> = self
                    .valid_metadata(now)
                    .flat_map(|entry| {
                        entry.state_labels.iter().filter_map(|(state, label)| {
                            Some((
                                *entry.state_label_reported_at.get(state)?,
                                state.clone(),
                                label.clone(),
                            ))
                        })
                    })
                    .collect();
                labels.sort_by_key(|(reported_at, _, _)| *reported_at);
                labels
                    .into_iter()
                    .map(|(_, state, label)| (state, label))
                    .collect()
            },
            metadata: self
                .user_metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.value.clone()))
                .collect(),
            agent_session: self.effective_session(),
            blocked_reason: self.blocked_reason(),
            cwd: Some(self.cwd.clone()),
            terminal: self.terminal.clone(),
            pid: self.pid,
            revision: self.revision,
            status_since_ms: self.status_since_ms,
        };

        let previous_status = self.last_info.as_ref().map(|last| last.agent_status);
        if previous_status != Some(status) {
            self.status_since_ms = now_epoch_ms();
            info.status_since_ms = self.status_since_ms;
        }
        let changed = self.last_info.as_ref() != Some(&info);
        if changed {
            self.revision += 1;
            info.revision = self.revision;
            self.last_info = Some(info.clone());
        }
        (info, changed)
    }
}

fn accept_seq(sequences: &mut HashMap<String, u64>, source: &str, seq: Option<u64>) -> bool {
    let Some(seq) = seq else {
        return true;
    };
    if sequences
        .get(source)
        .is_some_and(|last_seq| seq <= *last_seq)
    {
        return false;
    }
    sequences.insert(source.to_string(), seq);
    true
}

fn normalize_agent_label(agent: &str) -> String {
    parse_agent_label(agent)
        .map(|agent| agent_label(agent).to_string())
        .unwrap_or_else(|| agent.trim().to_string())
}

fn normalize_text(text: Option<String>, max_chars: usize) -> Option<String> {
    let trimmed = text?.trim().to_string();
    let mut normalized = String::new();
    for ch in trimmed.chars().filter(|ch| !ch.is_control()).take(max_chars) {
        normalized.push(ch);
    }
    let normalized = normalized.trim().to_string();
    (!normalized.is_empty()).then_some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> AgentEntry {
        AgentEntry::new(
            1,
            None,
            Some("claude".to_string()),
            "/tmp".to_string(),
            42,
            None,
        )
    }

    fn hook_report(state: &str) -> AgentReportAgentParams {
        AgentReportAgentParams {
            agent_id: "agent_1".to_string(),
            source: "shepherd:claude".to_string(),
            agent: "claude".to_string(),
            state: state.to_string(),
            message: None,
            custom_status: None,
            seq: None,
            agent_session_id: None,
            agent_session_path: None,
        }
    }

    #[test]
    fn new_entry_is_idle_and_seen() {
        let mut entry = entry();
        let (info, changed) = entry.snapshot();
        assert!(changed);
        assert_eq!(info.agent_status, AgentStatus::Idle);
        assert_eq!(info.agent.as_deref(), Some("claude"));
    }

    #[test]
    fn hook_authority_overrides_fallback_state() {
        let mut entry = entry();
        let now = Instant::now();
        entry.set_detected(Some(Agent::Claude), AgentState::Idle, false, false, now);
        entry.set_hook_authority(hook_report("working"), now);
        let (info, _) = entry.snapshot();
        assert_eq!(info.agent_status, AgentStatus::Working);
    }

    #[test]
    fn fresh_visible_blocker_overrides_non_blocked_hook() {
        let mut entry = entry();
        let now = Instant::now();
        entry.set_hook_authority(hook_report("working"), now);
        entry.set_detected(
            Some(Agent::Claude),
            AgentState::Blocked,
            true,
            false,
            now + Duration::from_millis(10),
        );
        let (info, _) = entry.snapshot();
        assert_eq!(info.agent_status, AgentStatus::Blocked);
    }

    #[test]
    fn idle_transition_derives_done_until_seen() {
        let mut entry = entry();
        let now = Instant::now();
        entry.set_detected(Some(Agent::Claude), AgentState::Working, false, false, now);
        let (info, _) = entry.snapshot();
        assert_eq!(info.agent_status, AgentStatus::Working);

        entry.set_detected(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            false,
            now + Duration::from_secs(1),
        );
        let (info, changed) = entry.snapshot();
        assert!(changed);
        assert_eq!(info.agent_status, AgentStatus::Done);

        entry.mark_seen();
        let (info, changed) = entry.snapshot();
        assert!(changed);
        assert_eq!(info.agent_status, AgentStatus::Idle);
    }

    #[test]
    fn session_report_survives_hook_clear() {
        let mut entry = entry();
        entry.set_session(AgentReportSessionParams {
            agent_id: "agent_1".to_string(),
            source: "shepherd:claude".to_string(),
            agent: "claude".to_string(),
            seq: Some(1),
            agent_session_id: Some("s-123".to_string()),
            agent_session_path: Some("/tmp/transcript.jsonl".to_string()),
        });
        let (info, _) = entry.snapshot();
        let session = info.agent_session.expect("session should be reported");
        assert_eq!(session.value, "s-123");
        assert_eq!(session.kind, "session_id");
    }

    #[test]
    fn stale_seq_reports_are_dropped() {
        let mut entry = entry();
        let now = Instant::now();
        let mut report = hook_report("working");
        report.seq = Some(10);
        entry.set_hook_authority(report, now);

        let mut stale = hook_report("idle");
        stale.seq = Some(5);
        entry.set_hook_authority(stale, now + Duration::from_millis(10));

        let (info, _) = entry.snapshot();
        assert_eq!(info.agent_status, AgentStatus::Working);
    }

    #[test]
    fn metadata_ttl_expires_custom_status() {
        let mut entry = entry();
        let now = Instant::now();
        entry.set_metadata(
            AgentReportMetadataParams {
                agent_id: "agent_1".to_string(),
                source: "shepherd:test".to_string(),
                agent: None,
                title: None,
                display_agent: None,
                custom_status: Some("thinking".to_string()),
                state_labels: HashMap::new(),
                clear_title: false,
                clear_display_agent: false,
                clear_custom_status: false,
                clear_state_labels: false,
                ttl_ms: Some(0),
                seq: None,
            },
            now - Duration::from_secs(1),
        );
        assert!(entry.has_ttl_metadata());
        let (info, _) = entry.snapshot();
        assert_eq!(info.custom_status, None);
    }

    #[test]
    fn user_metadata_set_update_remove_roundtrip() {
        let mut entry = entry();
        entry
            .set_user_metadata(
                [("jira".to_string(), "PROJ-1".to_string())].into(),
            )
            .expect("set should succeed");
        let (info, _) = entry.snapshot();
        assert_eq!(info.metadata.get("jira").map(String::as_str), Some("PROJ-1"));

        entry
            .set_user_metadata(
                [("jira".to_string(), "PROJ-2".to_string())].into(),
            )
            .expect("update should succeed");
        let (info, _) = entry.snapshot();
        assert_eq!(info.metadata.get("jira").map(String::as_str), Some("PROJ-2"));

        let removed = entry
            .set_user_metadata([("jira".to_string(), String::new())].into())
            .expect("remove should succeed");
        assert_eq!(removed, vec!["jira".to_string()]);
        let (info, _) = entry.snapshot();
        assert!(info.metadata.is_empty());
    }

    #[test]
    fn user_metadata_rejects_bad_input_without_applying() {
        let mut entry = entry();
        let result = entry.set_user_metadata(
            [
                ("ok".to_string(), "fine".to_string()),
                ("".to_string(), "bad key".to_string()),
            ]
            .into(),
        );
        assert!(result.is_err());
        assert!(entry.user_metadata().is_empty(), "all-or-nothing");

        assert!(entry.set_user_metadata(HashMap::new()).is_err());
        assert!(entry
            .set_user_metadata([("v".to_string(), "x".repeat(600))].into())
            .is_err());
    }

    #[test]
    fn session_store_key_uses_session_ref() {
        let mut entry = entry();
        assert_eq!(entry.session_store_key(), None);
        entry.set_session(AgentReportSessionParams {
            agent_id: "agent_1".to_string(),
            source: "shepherd:claude".to_string(),
            agent: "claude".to_string(),
            seq: None,
            agent_session_id: Some("s-9".to_string()),
            agent_session_path: None,
        });
        assert_eq!(entry.session_store_key().as_deref(), Some("session_id:s-9"));
    }

    #[test]
    fn custom_status_is_capped_at_32_chars() {
        let mut entry = entry();
        let mut report = hook_report("working");
        report.custom_status = Some("x".repeat(100));
        entry.set_hook_authority(report, Instant::now());
        let (info, _) = entry.snapshot();
        assert_eq!(info.custom_status.expect("status should be set").len(), 32);
    }
}
