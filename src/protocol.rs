//! Wire types shared by the ingest socket, the HTTP/WS monitor API, the
//! wrapper, and hook scripts. JSON-lines request/response on the ingest
//! socket; the same `AgentInfo`/`Event` shapes on the monitor side.
//!
//! Request/response shape and the `AgentInfo` field set follow herdr's
//! socket API (https://github.com/ogulcancelik/herdr, AGPL-3.0-or-later),
//! trimmed of pane/workspace coordinates.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub const DEFAULT_HTTP_PORT: u16 = 4650;

pub fn socket_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("SHEPHERD_SOCKET_PATH") {
        if !path.is_empty() {
            return path.into();
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::Path::new(&home)
        .join(".shepherd")
        .join("shepherd.sock")
}

pub fn http_port() -> u16 {
    std::env::var("SHEPHERD_HTTP_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_HTTP_PORT)
}

// ---------------------------------------------------------------------------
// Ingest requests (unix socket, one JSON object per line)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(flatten)]
    pub method: Method,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Method {
    /// Wrapper announces a new agent. The agent lives as long as the
    /// registering connection: when the connection drops, the agent is
    /// removed.
    #[serde(rename = "agent.register")]
    AgentRegister(AgentRegisterParams),
    /// Wrapper reports a screen-detection result.
    #[serde(rename = "agent.report_detection")]
    AgentReportDetection(AgentReportDetectionParams),
    /// Wrapper reports local user input (Seen evidence).
    #[serde(rename = "agent.seen")]
    AgentSeen(AgentTarget),
    /// Update an agent's display name (e.g. propagated from Claude Code's
    /// /rename). Latest write wins. Mirrors herdr's agent.rename.
    #[serde(rename = "agent.rename")]
    AgentRename(AgentRenameParams),
    /// Hook reports agent state (hook authority). Mirrors herdr's
    /// pane.report_agent.
    #[serde(rename = "agent.report_agent")]
    AgentReportAgent(AgentReportAgentParams),
    /// Hook reports a resumable agent session. Mirrors herdr's
    /// pane.report_agent_session.
    #[serde(rename = "agent.report_session")]
    AgentReportSession(AgentReportSessionParams),
    /// Hook reports display metadata. Mirrors herdr's pane.report_metadata.
    #[serde(rename = "agent.report_metadata")]
    AgentReportMetadata(AgentReportMetadataParams),
    /// Hook clears its authority. Mirrors herdr's pane.clear_agent_authority.
    #[serde(rename = "agent.clear_authority")]
    AgentClearAuthority(AgentClearAuthorityParams),
    /// Set durable user metadata on an agent (`shep meta` / the shep-meta
    /// skill). An empty value removes the key.
    #[serde(rename = "agent.set_metadata")]
    AgentSetMetadata(AgentSetMetadataParams),
    /// Snapshot of all agents (local clients, e.g. `shep status`).
    #[serde(rename = "agent.list")]
    AgentList(EmptyParams),
    /// Switch this connection into an event stream: snapshot event first,
    /// then agent_added/agent_updated/agent_removed lines as they happen.
    #[serde(rename = "events.subscribe")]
    EventsSubscribe(EmptyParams),
    #[serde(rename = "ping")]
    Ping(EmptyParams),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmptyParams {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTarget {
    pub agent_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRenameParams {
    pub agent_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegisterParams {
    /// Requested id on re-registration after a reconnect (the wrapper's
    /// original id, baked into the child's SHEPHERD_AGENT_ID). None on
    /// first registration. The server honors it when free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Canonical agent label if the wrapper identified one from argv.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub argv: Vec<String>,
    pub cwd: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalLocation>,
}

/// Identity of the terminal a Wrapper runs in. Captured from the wrapper's
/// environment at registration; immutable for the agent's lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalLocation {
    pub app: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReportDetectionParams {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub state: String,
    #[serde(default)]
    pub visible_blocker: bool,
    #[serde(default)]
    pub visible_working: bool,
    #[serde(default)]
    pub process_exited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReportAgentParams {
    pub agent_id: String,
    pub source: String,
    pub agent: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReportSessionParams {
    pub agent_id: String,
    pub source: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReportMetadataParams {
    pub agent_id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_status: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub state_labels: HashMap<String, String>,
    #[serde(default)]
    pub clear_title: bool,
    #[serde(default)]
    pub clear_display_agent: bool,
    #[serde(default)]
    pub clear_custom_status: bool,
    #[serde(default)]
    pub clear_state_labels: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSetMetadataParams {
    pub agent_id: String,
    pub entries: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentClearAuthorityParams {
    pub agent_id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

// ---------------------------------------------------------------------------
// Responses and events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event: String,
    pub data: serde_json::Value,
}

pub const EVENT_SNAPSHOT: &str = "snapshot";
pub const EVENT_AGENT_ADDED: &str = "agent_added";
pub const EVENT_AGENT_UPDATED: &str = "agent_updated";
pub const EVENT_AGENT_REMOVED: &str = "agent_removed";

// ---------------------------------------------------------------------------
// Monitor-facing agent snapshot
// ---------------------------------------------------------------------------

/// The Monitor-facing status: detected Agent State plus the derived Done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionInfo {
    pub source: String,
    pub agent: String,
    pub kind: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub agent_status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_status: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub state_labels: HashMap<String, String>,
    /// User/agent-written session metadata (sibling of state_labels; durable).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<AgentSessionInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalLocation>,
    pub pid: u32,
    pub revision: u64,
    /// Epoch milliseconds of the last agent_status change.
    pub status_since_ms: u64,
}

pub fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_use_dot_method_names() {
        let request: Request = serde_json::from_str(
            r#"{"id":1,"method":"agent.report_session","params":{"agent_id":"a1","source":"shepherd:claude","agent":"claude","agent_session_id":"s-123"}}"#,
        )
        .expect("request should parse");
        match request.method {
            Method::AgentReportSession(params) => {
                assert_eq!(params.agent_id, "a1");
                assert_eq!(params.agent_session_id.as_deref(), Some("s-123"));
            }
            other => panic!("unexpected method: {other:?}"),
        }
    }

    #[test]
    fn set_metadata_request_parses() {
        let request: Request = serde_json::from_str(
            r#"{"id":2,"method":"agent.set_metadata","params":{"agent_id":"agent_1","entries":{"jira":"PROJ-123","stale":""}}}"#,
        )
        .expect("request should parse");
        match request.method {
            Method::AgentSetMetadata(params) => {
                assert_eq!(params.agent_id, "agent_1");
                assert_eq!(params.entries.get("jira").map(String::as_str), Some("PROJ-123"));
                assert_eq!(params.entries.get("stale").map(String::as_str), Some(""));
            }
            other => panic!("unexpected method: {other:?}"),
        }
    }

    #[test]
    fn register_params_agent_id_is_optional_and_omitted_when_none() {
        // Old wrappers omit agent_id entirely.
        let request: Request = serde_json::from_str(
            r#"{"id":1,"method":"agent.register","params":{"argv":["claude"],"cwd":"/w","pid":1}}"#,
        )
        .expect("request should parse");
        match request.method {
            Method::AgentRegister(params) => assert!(params.agent_id.is_none()),
            other => panic!("unexpected method: {other:?}"),
        }

        // Re-registration carries the retained id.
        let request: Request = serde_json::from_str(
            r#"{"id":1,"method":"agent.register","params":{"agent_id":"agent_7","argv":["claude"],"cwd":"/w","pid":1}}"#,
        )
        .expect("request should parse");
        match request.method {
            Method::AgentRegister(params) => {
                assert_eq!(params.agent_id.as_deref(), Some("agent_7"));
                // None serializes to an absent field (wire-compatible).
                let mut params = params;
                params.agent_id = None;
                let json = serde_json::to_string(&params).expect("params should serialize");
                assert!(!json.contains("agent_id"), "unexpected field in {json}");
            }
            other => panic!("unexpected method: {other:?}"),
        }
    }

    #[test]
    fn agent_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&AgentStatus::Done).expect("status should serialize"),
            "\"done\""
        );
    }
}
