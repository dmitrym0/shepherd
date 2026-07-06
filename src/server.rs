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

const INDEX_HTML: &str = include_str!("web/index.html");

struct Registry {
    agents: HashMap<u64, AgentEntry>,
    next_id: u64,
    events: broadcast::Sender<String>,
}

impl Registry {
    fn new(events: broadcast::Sender<String>) -> Self {
        Self {
            agents: HashMap::new(),
            next_id: 1,
            events,
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
        let id = self.next_id;
        self.next_id += 1;
        let mut entry = AgentEntry::new(
            id,
            entry_fields.name,
            entry_fields.agent,
            entry_fields.cwd,
            entry_fields.pid,
        );
        let (info, _) = entry.snapshot();
        self.agents.insert(id, entry);
        self.emit(EVENT_AGENT_ADDED, serde_json::to_value(info).expect("info should serialize"));
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
        self.registry.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub async fn serve() -> std::io::Result<()> {
    let (events, _) = broadcast::channel(1024);
    let shared = Shared {
        registry: Arc::new(Mutex::new(Registry::new(events.clone()))),
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
                    format!("a shepherd server is already listening at {}", path.display()),
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
        Method::AgentReportAgent(params) => {
            let agent_id = params.agent_id.clone();
            shared.lock().update_agent(&agent_id, |entry| {
                entry.set_hook_authority(params, Instant::now());
            })?;
            Ok(serde_json::json!({}))
        }
        Method::AgentReportSession(params) => {
            let agent_id = params.agent_id.clone();
            shared
                .lock()
                .update_agent(&agent_id, |entry| entry.set_session(params))?;
            Ok(serde_json::json!({}))
        }
        Method::AgentReportMetadata(params) => {
            let agent_id = params.agent_id.clone();
            shared.lock().update_agent(&agent_id, |entry| {
                entry.set_metadata(params, Instant::now());
            })?;
            Ok(serde_json::json!({}))
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
