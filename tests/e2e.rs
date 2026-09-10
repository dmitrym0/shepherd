//! End-to-end: real server process, ingest socket protocol, and the wrapper.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ServerHandle {
    child: Child,
    socket: PathBuf,
    _dir: PathBuf,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_server(tag: &str) -> ServerHandle {
    let dir = std::env::temp_dir().join(format!("shepherd-e2e-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir should be created");
    let socket = dir.join("shepherd.sock");
    // Distinct port per server; the tests below only use the socket.
    static PORT_OFFSET: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let offset = PORT_OFFSET.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let port = 20000 + (std::process::id() % 10000) as u16 + offset;
    let child = Command::new(env!("CARGO_BIN_EXE_shep"))
        .arg("serve")
        .env("SHEPHERD_SOCKET_PATH", &socket)
        .env("SHEPHERD_HTTP_PORT", port.to_string())
        // Hermetic ~/.shepherd: the server persists metadata under HOME.
        .env("HOME", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("server should spawn");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "server should create its socket");
    ServerHandle {
        child,
        socket,
        _dir: dir,
    }
}

fn send_request(
    stream: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
    request: serde_json::Value,
) -> serde_json::Value {
    let mut line = request.to_string();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .expect("request should send");
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .expect("response should arrive");
    serde_json::from_str(&response).expect("response should be JSON")
}

fn connect(socket: &PathBuf) -> (UnixStream, BufReader<UnixStream>) {
    let stream = UnixStream::connect(socket).expect("socket should connect");
    let reader = BufReader::new(stream.try_clone().expect("stream should clone"));
    (stream, reader)
}

fn list_agents(socket: &PathBuf) -> Vec<serde_json::Value> {
    let (mut stream, mut reader) = connect(socket);
    let response = send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({"id": 1, "method": "agent.list", "params": {}}),
    );
    response["result"]["agents"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn register_report_list_and_removal_on_disconnect() {
    let server = start_server("core");

    let (mut stream, mut reader) = connect(&server.socket);
    let response = send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 1,
            "method": "agent.register",
            "params": {"name": "demo", "agent": "claude", "argv": ["claude"], "cwd": "/tmp", "pid": 123}
        }),
    );
    let agent_id = response["result"]["agent_id"]
        .as_str()
        .expect("register should return an agent id")
        .to_string();

    // Fresh registration reads Idle.
    let agents = list_agents(&server.socket);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["agent_status"], "idle");
    assert_eq!(agents[0]["agent"], "claude");
    assert_eq!(agents[0]["name"], "demo");

    // Screen detection: working.
    let response = send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 2,
            "method": "agent.report_detection",
            "params": {"agent_id": agent_id, "agent": "claude", "state": "working", "visible_working": true}
        }),
    );
    assert!(
        response["error"].is_null(),
        "report should succeed: {response}"
    );
    let agents = list_agents(&server.socket);
    assert_eq!(agents[0]["agent_status"], "working");

    // Back to idle without a Seen report: Done.
    send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 3,
            "method": "agent.report_detection",
            "params": {"agent_id": agent_id, "agent": "claude", "state": "idle", "visible_blocker": false}
        }),
    );
    let agents = list_agents(&server.socket);
    assert_eq!(agents[0]["agent_status"], "done");

    // Seen flips Done back to Idle.
    send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({"id": 4, "method": "agent.seen", "params": {"agent_id": agent_id}}),
    );
    let agents = list_agents(&server.socket);
    assert_eq!(agents[0]["agent_status"], "idle");

    // Hook session report surfaces the resumable session.
    send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 5,
            "method": "agent.report_session",
            "params": {"agent_id": agent_id, "source": "shepherd:claude", "agent": "claude", "seq": 1, "agent_session_id": "sess-9", "agent_session_path": "/tmp/t.jsonl"}
        }),
    );
    let agents = list_agents(&server.socket);
    assert_eq!(agents[0]["agent_session"]["value"], "sess-9");

    // Rename (e.g. propagated from Claude's /rename) overwrites the name.
    send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({"id": 6, "method": "agent.rename", "params": {"agent_id": agent_id, "name": "renamed"}}),
    );
    let agents = list_agents(&server.socket);
    assert_eq!(agents[0]["name"], "renamed");

    // Dropping the registering connection removes the agent.
    drop(stream);
    drop(reader);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if list_agents(&server.socket).is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "agent should be removed when its connection drops"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn events_subscribe_streams_snapshot_and_updates() {
    let server = start_server("events");

    let (mut subscriber, mut subscriber_reader) = connect(&server.socket);
    let response = send_request(
        &mut subscriber,
        &mut subscriber_reader,
        serde_json::json!({"id": 1, "method": "events.subscribe", "params": {}}),
    );
    assert_eq!(response["result"]["subscribed"], true);
    let mut line = String::new();
    subscriber_reader
        .read_line(&mut line)
        .expect("snapshot event should arrive");
    let event: serde_json::Value = serde_json::from_str(&line).expect("event should be JSON");
    assert_eq!(event["event"], "snapshot");

    let (mut stream, mut reader) = connect(&server.socket);
    send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 1,
            "method": "agent.register",
            "params": {"argv": ["claude"], "agent": "claude", "cwd": "/tmp", "pid": 1}
        }),
    );

    line.clear();
    subscriber_reader
        .read_line(&mut line)
        .expect("agent_added event should arrive");
    let event: serde_json::Value = serde_json::from_str(&line).expect("event should be JSON");
    assert_eq!(event["event"], "agent_added");
    assert_eq!(event["data"]["agent_status"], "idle");
}

#[test]
fn metadata_survives_server_restart() {
    let server = start_server("durable");
    let socket_path = server.socket.clone();

    let (mut stream, mut reader) = connect(&server.socket);
    let response = send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 1,
            "method": "agent.register",
            "params": {"name": "durable", "agent": "claude", "argv": ["claude"], "cwd": "/tmp", "pid": 7}
        }),
    );
    let agent_id = response["result"]["agent_id"]
        .as_str()
        .expect("register should return an agent id")
        .to_string();

    // Metadata written before the session id is known stays visible...
    let response = send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 2,
            "method": "agent.set_metadata",
            "params": {"agent_id": agent_id, "entries": {"jira": "PROJ-9"}}
        }),
    );
    assert!(response["error"].is_null(), "set should succeed: {response}");
    let agents = list_agents(&server.socket);
    assert_eq!(agents[0]["metadata"]["jira"], "PROJ-9");

    // ...and becomes durable once the resumable session id arrives.
    send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 3,
            "method": "agent.report_session",
            "params": {"agent_id": agent_id, "source": "shepherd:claude", "agent": "claude", "seq": 1, "agent_session_id": "sess-42"}
        }),
    );
    send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 4,
            "method": "agent.set_metadata",
            "params": {"agent_id": agent_id, "entries": {"description": "durability check"}}
        }),
    );

    // Unknown agents are rejected.
    let response = send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 5,
            "method": "agent.set_metadata",
            "params": {"agent_id": "agent_999", "entries": {"k": "v"}}
        }),
    );
    assert!(!response["error"].is_null(), "unknown agent should error");

    // Restart the server (same HOME, same socket path), resume the session.
    drop(stream);
    drop(reader);
    drop(server);
    let _ = std::fs::remove_file(&socket_path);
    let server = start_server("durable");

    let (mut stream, mut reader) = connect(&server.socket);
    let response = send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 1,
            "method": "agent.register",
            "params": {"name": "resumed", "agent": "claude", "argv": ["claude"], "cwd": "/tmp", "pid": 8}
        }),
    );
    let agent_id = response["result"]["agent_id"]
        .as_str()
        .expect("register should return an agent id")
        .to_string();
    let agents = list_agents(&server.socket);
    assert!(
        agents[0]["metadata"].is_null(),
        "no metadata before the session id reattaches"
    );

    send_request(
        &mut stream,
        &mut reader,
        serde_json::json!({
            "id": 2,
            "method": "agent.report_session",
            "params": {"agent_id": agent_id, "source": "shepherd:claude", "agent": "claude", "seq": 1, "agent_session_id": "sess-42"}
        }),
    );
    let agents = list_agents(&server.socket);
    assert_eq!(agents[0]["metadata"]["jira"], "PROJ-9");
    assert_eq!(agents[0]["metadata"]["description"], "durability check");
}

#[test]
fn wrapper_reconnects_after_server_restart_keeping_its_id() {
    let mut server = start_server("restart");

    let mut wrapper = Command::new(env!("CARGO_BIN_EXE_shep"))
        .args(["run", "--name", "phoenix", "--", "sh", "-c", "sleep 60"])
        .env("SHEPHERD_SOCKET_PATH", &server.socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("wrapper should spawn");

    // Registered on the first server.
    let deadline = Instant::now() + Duration::from_secs(5);
    let agent_id = loop {
        let agents = list_agents(&server.socket);
        if agents.len() == 1 {
            break agents[0]["agent_id"]
                .as_str()
                .expect("agent should have an id")
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "wrapper should register its agent"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    // Kill the server; the wrapper keeps running. Remove the stale socket
    // file so start_server's existence check waits for the new server.
    server.child.kill().expect("server should die");
    server.child.wait().expect("server should be reaped");
    std::fs::remove_file(&server.socket).ok();

    let server2 = start_server("restart");

    // The session reappears on its own, under the same id and name,
    // within the spec's 30 s bound.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let agents = list_agents(&server2.socket);
        if agents.len() == 1 {
            assert_eq!(agents[0]["agent_id"], agent_id.as_str());
            assert_eq!(agents[0]["name"], "phoenix");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "session should reappear after the server restart"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let _ = wrapper.kill();
    let _ = wrapper.wait();
}

#[test]
fn wrapper_runs_a_command_and_registers_it() {
    let server = start_server("wrapper");

    let mut wrapper = Command::new(env!("CARGO_BIN_EXE_shep"))
        .args(["run", "--name", "smoke", "--", "sh", "-c", "sleep 2"])
        .env("SHEPHERD_SOCKET_PATH", &server.socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("wrapper should spawn");

    // The agent appears while the command runs...
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let agents = list_agents(&server.socket);
        if agents.len() == 1 {
            assert_eq!(agents[0]["name"], "smoke");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "wrapper should register its agent"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // ...and disappears when it exits.
    let status = wrapper.wait().expect("wrapper should exit");
    assert!(status.success(), "wrapper should propagate a zero exit");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if list_agents(&server.socket).is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "agent should be removed after the wrapped command exits"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
