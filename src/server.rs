//! The shepherd server: aggregates wrapper/hook reports over a unix socket
//! and fans agent status out to monitors over HTTP + WebSocket. Owns no PTYs
//! and no terminals.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::detect::{parse_agent_label, AgentState};
use crate::protocol::{
    http_port, socket_path, AgentInfo, Event, Method, Request, Response, ResponseError,
    EVENT_AGENT_ADDED, EVENT_AGENT_REMOVED, EVENT_AGENT_UPDATED, EVENT_SNAPSHOT,
};
use crate::state::AgentEntry;
use crate::store::{merge_newest, Config, MetadataStore};

const INDEX_HTML: &str = include_str!("web/index.html");

struct Registry {
    agents: HashMap<u64, AgentEntry>,
    next_id: u64,
    events: broadcast::Sender<String>,
    store: MetadataStore,
}

impl Registry {
    fn new(events: broadcast::Sender<String>, store: MetadataStore) -> Self {
        Self {
            agents: HashMap::new(),
            // Epoch-seeded so ids issued after a server restart can never
            // collide with ids held by wrappers that re-register with the
            // id they got before the restart. ponytail: seeding beats
            // persisting next_id; a collision needs two registrations in
            // the same millisecond across a restart, and the occupied-id
            // fallback in register() absorbs even that.
            next_id: crate::protocol::now_epoch_ms(),
            events,
            store,
        }
    }

    fn emit(&self, event: &str, data: serde_json::Value) {
        let line = serde_json::to_string(&Event {
            event: event.to_string(),
            data,
        })
        .expect("event should serialize");
        let _ = self.events.send(line);
    }

    fn register(&mut self, entry_fields: crate::protocol::AgentRegisterParams) -> u64 {
        // A reconnecting wrapper asks for its original id back; honor it
        // when free (the response's agent_id stays authoritative either way).
        let requested = entry_fields
            .agent_id
            .as_deref()
            .and_then(|id| parse_agent_id(id).ok())
            .filter(|id| !self.agents.contains_key(id));
        let id = if let Some(id) = requested {
            self.next_id = self.next_id.max(id + 1);
            id
        } else {
            let id = self.next_id;
            self.next_id += 1;
            id
        };
        let mut entry = AgentEntry::new(
            id,
            entry_fields.name,
            entry_fields.agent,
            entry_fields.cwd,
            entry_fields.pid,
            entry_fields.terminal,
        );
        let (info, _) = entry.snapshot();
        self.agents.insert(id, entry);
        self.emit(
            EVENT_AGENT_ADDED,
            serde_json::to_value(info).expect("info should serialize"),
        );
        id
    }

    fn remove(&mut self, id: u64) {
        if self.agents.remove(&id).is_some() {
            self.emit(
                EVENT_AGENT_REMOVED,
                serde_json::json!({ "agent_id": format!("agent_{id}") }),
            );
        }
    }

    fn snapshot_all(&mut self) -> Vec<AgentInfo> {
        let mut infos: Vec<AgentInfo> = self
            .agents
            .values_mut()
            .map(|entry| entry.snapshot().0)
            .collect();
        infos.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        infos
    }

    /// Run a mutation against one agent, then emit agent_updated if the
    /// monitor-facing snapshot changed.
    fn update_agent(
        &mut self,
        agent_id: &str,
        mutate: impl FnOnce(&mut AgentEntry),
    ) -> Result<(), String> {
        let id = parse_agent_id(agent_id)?;
        let entry = self
            .agents
            .get_mut(&id)
            .ok_or_else(|| format!("unknown agent: {agent_id}"))?;
        mutate(entry);
        let (info, changed) = entry.snapshot();
        if changed {
            self.emit(
                EVENT_AGENT_UPDATED,
                serde_json::to_value(info).expect("info should serialize"),
            );
        }
        Ok(())
    }

    /// Apply a user metadata write, reconcile with the durable store, emit.
    fn set_user_metadata(
        &mut self,
        agent_id: &str,
        entries: HashMap<String, String>,
    ) -> Result<(), String> {
        let id = parse_agent_id(agent_id)?;
        let live = self.agents.len();
        let entry = self
            .agents
            .get_mut(&id)
            .ok_or_else(|| format!("unknown agent: {agent_id} ({live} live)"))?;
        let removed = entry.set_user_metadata(entries)?;
        self.sync_metadata(id, &removed);
        self.update_agent(agent_id, |_| {})
    }

    /// Reconcile one entry's metadata with the durable store and persist:
    /// per-key newest wins across both, except keys the current write
    /// removed, which stay removed. No-op until the agent has a resumable
    /// session (pre-identity metadata lives only on the entry).
    fn sync_metadata(&mut self, id: u64, removed: &[String]) {
        let Some(entry) = self.agents.get_mut(&id) else {
            return;
        };
        let Some(session_key) = entry.session_store_key() else {
            return;
        };
        let mut merged = entry.user_metadata().clone();
        if let Some(stored) = self.store.get(&session_key) {
            merge_newest(&mut merged, stored);
        }
        for key in removed {
            merged.remove(key);
        }
        entry.replace_user_metadata(merged.clone());
        self.store.replace(&session_key, merged);
    }

    /// Re-attach stored metadata after a session identity arrives (resume,
    /// restart, or first hook report), then emit if the snapshot changed.
    fn resync_metadata(&mut self, agent_id: &str) {
        let Ok(id) = parse_agent_id(agent_id) else {
            return;
        };
        self.sync_metadata(id, &[]);
        let _ = self.update_agent(agent_id, |_| {});
    }
}

fn parse_agent_id(agent_id: &str) -> Result<u64, String> {
    agent_id
        .strip_prefix("agent_")
        .and_then(|raw| raw.parse().ok())
        .ok_or_else(|| format!("invalid agent id: {agent_id}"))
}

#[derive(Clone)]
struct Shared {
    registry: Arc<Mutex<Registry>>,
    events: broadcast::Sender<String>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub async fn serve() -> std::io::Result<()> {
    let (events, _) = broadcast::channel(1024);
    let shared = Shared {
        registry: Arc::new(Mutex::new(Registry::new(
            events.clone(),
            MetadataStore::load(MetadataStore::default_path()),
        ))),
        events,
    };

    let path = socket_path();
    let listener = bind_ingest_socket(&path)?;
    eprintln!("shepherd: ingest socket at {}", path.display());

    let ingest_shared = shared.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let shared = ingest_shared.clone();
                    tokio::spawn(async move {
                        handle_ingest_connection(stream, shared).await;
                    });
                }
                Err(err) => {
                    eprintln!("shepherd: ingest accept error: {err}");
                }
            }
        }
    });

    // Metadata TTLs can hide presentation fields without any new report
    // arriving; sweep entries that carry TTLs so monitors see the expiry.
    let ttl_shared = shared.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tick.tick().await;
            let mut registry = ttl_shared.lock();
            let ids: Vec<u64> = registry
                .agents
                .iter()
                .filter(|(_, entry)| entry.has_ttl_metadata())
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                let _ = registry.update_agent(&format!("agent_{id}"), |_| {});
            }
        }
    });

    let app = axum::Router::new()
        .route("/", get(index))
        .route("/agents", get(agents))
        .route("/config", get(config))
        .route("/ws", get(ws_upgrade))
        .with_state(shared);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], http_port()));
    let http = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("shepherd: monitor at http://{addr}");
    axum::serve(http, app).await
}

fn bind_ingest_socket(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "a shepherd server is already listening at {}",
                        path.display()
                    ),
                ));
            }
            Err(_) => {
                // Stale socket from a dead server.
                std::fs::remove_file(path)?;
            }
        }
    }
    UnixListener::bind(path)
}

async fn handle_ingest_connection(stream: UnixStream, shared: Shared) {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    // The agent registered on this connection; removed when the
    // connection drops (wrapper death == agent death).
    let mut registered_agent: Option<u64> = None;

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(err) => {
                let response = Response {
                    id: None,
                    result: None,
                    error: Some(ResponseError {
                        message: format!("invalid request: {err}"),
                    }),
                };
                if write_line(&mut write_half, &response).await.is_err() {
                    break;
                }
                continue;
            }
        };

        let id = request.id.clone();
        let subscribe = matches!(request.method, Method::EventsSubscribe(_));
        let result = handle_request(request, &shared, &mut registered_agent);
        let response = match result {
            Ok(result) => Response {
                id,
                result: Some(result),
                error: None,
            },
            Err(message) => Response {
                id,
                result: None,
                error: Some(ResponseError { message }),
            },
        };
        if write_line(&mut write_half, &response).await.is_err() {
            break;
        }

        if subscribe {
            stream_events(&mut write_half, &shared).await;
            break;
        }
    }

    if let Some(id) = registered_agent {
        shared.lock().remove(id);
    }
}

fn handle_request(
    request: Request,
    shared: &Shared,
    registered_agent: &mut Option<u64>,
) -> Result<serde_json::Value, String> {
    match request.method {
        Method::Ping(_) => Ok(serde_json::json!({"pong": true})),
        Method::AgentRegister(params) => {
            let id = shared.lock().register(params);
            *registered_agent = Some(id);
            Ok(serde_json::json!({ "agent_id": format!("agent_{id}") }))
        }
        Method::AgentReportDetection(params) => {
            let state = AgentState::parse(&params.state)
                .ok_or_else(|| format!("invalid state: {}", params.state))?;
            let agent = params.agent.as_deref().and_then(parse_agent_label);
            shared.lock().update_agent(&params.agent_id, |entry| {
                entry.set_detected(
                    agent,
                    state,
                    params.visible_blocker,
                    params.process_exited,
                    Instant::now(),
                );
            })?;
            Ok(serde_json::json!({}))
        }
        Method::AgentSeen(params) => {
            shared
                .lock()
                .update_agent(&params.agent_id, |entry| entry.mark_seen())?;
            Ok(serde_json::json!({}))
        }
        Method::AgentRename(params) => {
            shared
                .lock()
                .update_agent(&params.agent_id, |entry| entry.set_name(&params.name))?;
            Ok(serde_json::json!({}))
        }
        Method::AgentReportAgent(params) => {
            let agent_id = params.agent_id.clone();
            let mut registry = shared.lock();
            registry.update_agent(&agent_id, |entry| {
                entry.set_hook_authority(params, Instant::now());
            })?;
            registry.resync_metadata(&agent_id);
            Ok(serde_json::json!({}))
        }
        Method::AgentReportSession(params) => {
            let agent_id = params.agent_id.clone();
            let mut registry = shared.lock();
            registry.update_agent(&agent_id, |entry| entry.set_session(params))?;
            registry.resync_metadata(&agent_id);
            Ok(serde_json::json!({}))
        }
        Method::AgentReportMetadata(params) => {
            let agent_id = params.agent_id.clone();
            shared.lock().update_agent(&agent_id, |entry| {
                entry.set_metadata(params, Instant::now());
            })?;
            Ok(serde_json::json!({}))
        }
        Method::AgentReportActivity(params) => {
            let agent_id = params.agent_id.clone();
            shared
                .lock()
                .update_agent(&agent_id, |entry| entry.set_activity(params.activity))?;
            Ok(serde_json::json!({}))
        }
        Method::AgentSetMetadata(params) => {
            shared
                .lock()
                .set_user_metadata(&params.agent_id, params.entries)?;
            Ok(serde_json::json!({"ok": true}))
        }
        Method::AgentClearAuthority(params) => {
            shared.lock().update_agent(&params.agent_id, |entry| {
                entry.clear_authority(&params.source, params.seq);
            })?;
            Ok(serde_json::json!({}))
        }
        Method::AgentList(_) => {
            let agents = shared.lock().snapshot_all();
            Ok(serde_json::json!({ "agents": agents }))
        }
        Method::EventsSubscribe(_) => Ok(serde_json::json!({"subscribed": true})),
    }
}

async fn write_line(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    value: &impl serde::Serialize,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(value).expect("value should serialize");
    line.push('\n');
    write_half.write_all(line.as_bytes()).await
}

async fn stream_events(write_half: &mut tokio::net::unix::OwnedWriteHalf, shared: &Shared) {
    // Subscribe before the snapshot so nothing falls between them; events
    // that duplicate snapshot content are harmless (full-object payloads).
    let mut rx = shared.events.subscribe();
    let snapshot = snapshot_event(shared);
    if write_half
        .write_all(format!("{snapshot}\n").as_bytes())
        .await
        .is_err()
    {
        return;
    }
    loop {
        match rx.recv().await {
            Ok(line) => {
                if write_half
                    .write_all(format!("{line}\n").as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let snapshot = snapshot_event(shared);
                if write_half
                    .write_all(format!("{snapshot}\n").as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn snapshot_event(shared: &Shared) -> String {
    let agents = shared.lock().snapshot_all();
    serde_json::to_string(&Event {
        event: EVENT_SNAPSHOT.to_string(),
        data: serde_json::json!({ "agents": agents }),
    })
    .expect("snapshot should serialize")
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn agents(State(shared): State<Shared>) -> Json<Vec<AgentInfo>> {
    Json(shared.lock().snapshot_all())
}

async fn config() -> Json<Config> {
    Json(Config::load())
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(shared): State<Shared>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, shared))
}

async fn handle_ws(mut socket: WebSocket, shared: Shared) {
    let mut rx = shared.events.subscribe();
    let snapshot = snapshot_event(&shared);
    if socket.send(Message::Text(snapshot.into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(line) => {
                        if socket.send(Message::Text(line.into())).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let snapshot = snapshot_event(&shared);
                        if socket.send(Message::Text(snapshot.into())).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
            message = socket.recv() => {
                match message {
                    // Ignore client messages; monitors are read-only.
                    Some(Ok(_)) => {}
                    _ => return,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        // A nonexistent path loads as an empty store; these tests never
        // write metadata, so nothing is persisted.
        let store = crate::store::MetadataStore::load(
            std::env::temp_dir().join(format!("shep-registry-test-{}.json", std::process::id())),
        );
        Registry::new(broadcast::channel(8).0, store)
    }

    fn params(agent_id: Option<&str>) -> crate::protocol::AgentRegisterParams {
        crate::protocol::AgentRegisterParams {
            agent_id: agent_id.map(str::to_string),
            name: None,
            agent: Some("claude".to_string()),
            argv: vec!["claude".to_string()],
            cwd: "/tmp".to_string(),
            pid: 1,
            terminal: None,
        }
    }

    #[test]
    fn next_id_is_epoch_seeded() {
        // Not starting from 1: a restarted server must never re-issue ids
        // that wrappers obtained before the restart.
        assert!(registry().next_id > 1_000_000);
    }

    #[test]
    fn register_honors_a_free_requested_id() {
        let mut registry = registry();
        assert_eq!(registry.register(params(Some("agent_42"))), 42);
        // next_id advanced past the requested id and fresh ids don't collide.
        let fresh = registry.register(params(None));
        assert_ne!(fresh, 42);
        assert!(registry.agents.contains_key(&42));
    }

    #[test]
    fn register_falls_back_when_requested_id_is_occupied() {
        let mut registry = registry();
        assert_eq!(registry.register(params(Some("agent_7"))), 7);
        let second = registry.register(params(Some("agent_7")));
        assert_ne!(second, 7);
        assert_eq!(registry.agents.len(), 2);
    }

    #[test]
    fn register_falls_back_on_malformed_requested_id() {
        let mut registry = registry();
        let seeded_next = registry.next_id;
        assert_eq!(registry.register(params(Some("banana"))), seeded_next);
    }
}
