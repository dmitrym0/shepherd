//! The `shep run` wrapper: a transparent PTY shim. Spawns the agent under
//! a PTY in the user's terminal, passes bytes through untouched, runs screen
//! detection on the output stream, and reports state to the server over the
//! ingest socket. The agent lives and dies with this process.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::detect::{parse_agent_label, Agent, AgentState};
use crate::protocol::{socket_path, Method, Request, Response};
use crate::supervise::{
    decide_detection_screen_read, decide_screen_detection_publish,
    detection_update_for_publish_with_osc, DetectionPublishDecision, DetectionScreenReadDecision,
    DetectionScreenReadInput, PendingIdleConfirmation, ScreenDetectionPublishInput,
    AGENT_PENDING_IDLE_RECHECK, AGENT_STARTUP_GRACE_WINDOW, DETECTION_INTERVAL,
};

const SEEN_REPORT_THROTTLE: Duration = Duration::from_secs(2);
const CLAUDE_SESSION_POLL: Duration = Duration::from_secs(2);
const VT_SCROLLBACK_LINES: usize = 500;

/// Keepalive cadence: a ping's failed write is how a session (idle ones
/// included) detects a dead server. With the reconnect ceiling below it
/// keeps reappearance well inside the spec's 30 s bound.
const PING_INTERVAL: Duration = Duration::from_secs(5);
const RECONNECT_INITIAL: Duration = Duration::from_millis(100);
const RECONNECT_CEILING: Duration = Duration::from_secs(5);

/// Terminal emulation state fed by the PTY reader, read by the detection loop.
struct Emulation {
    parser: vt100::Parser,
    osc: crate::osc::AgentOscStateTracker,
}

struct IngestClient {
    writer: Mutex<UnixStream>,
    /// Set by any failed write; cleared by the keepalive thread once a
    /// reconnect succeeds.
    broken: AtomicBool,
    /// Launch-time registration payload with `agent_id` filled in after the
    /// first registration — re-sent verbatim on every reconnect so the
    /// session keeps the id baked into the child's SHEPHERD_AGENT_ID.
    register_params: crate::protocol::AgentRegisterParams,
    /// Replay cache: the latest rename/detection sent, re-sent after a
    /// re-registration so the dashboard converges to current state instead
    /// of waiting for the next change. Nothing else is buffered — events
    /// during an outage are dropped by design.
    last_rename: Mutex<Option<Method>>,
    last_detection: Mutex<Option<Method>>,
    /// The agent's resumable session id. Replayed like the others, which is
    /// what makes identity survive a server restart: the fresh server learns
    /// it again without the session having to do anything (git-bug 69681aa).
    last_session: Mutex<Option<Method>>,
}

impl IngestClient {
    /// Fire-and-forget notification; responses are drained by a separate
    /// thread. Errors never surface to callers — a dead server must never
    /// break the user's terminal session — but they flag the connection
    /// broken so the keepalive thread reconnects.
    fn notify(&self, method: Method) {
        match &method {
            Method::AgentRename(_) => cache(&self.last_rename, &method),
            Method::AgentReportDetection(_) => cache(&self.last_detection, &method),
            Method::AgentReportSession(_) => cache(&self.last_session, &method),
            _ => {}
        }
        let request = Request { id: None, method };
        let mut line = serde_json::to_string(&request).expect("request should serialize");
        line.push('\n');
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if writer.write_all(line.as_bytes()).is_err() {
            self.broken.store(true, Ordering::Release);
        }
    }

    /// Current state to re-send after a re-registration.
    fn replay_methods(&self) -> Vec<Method> {
        [&self.last_rename, &self.last_detection, &self.last_session]
            .into_iter()
            .filter_map(|slot| {
                slot.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
            })
            .collect()
    }

    /// Blocks until the server is back: connect-only with exponential
    /// backoff — this path never starts a server; something else owns the
    /// server's lifecycle. On success, re-registers under the original
    /// agent id and replays current state.
    fn reconnect(&self, stop: &AtomicBool) {
        let path = socket_path();
        let mut delay = RECONNECT_INITIAL;
        loop {
            std::thread::sleep(delay);
            if stop.load(Ordering::Acquire) {
                return;
            }
            delay = next_backoff(delay);
            let Ok(mut stream) = UnixStream::connect(&path) else {
                continue;
            };
            let Ok(clone) = stream.try_clone() else {
                continue;
            };
            let mut reader = BufReader::new(clone);
            // ponytail: a mismatched id in the response (our id occupied on
            // the fresh server) is ignored — epoch-seeded server ids make
            // it unreachable, and the child's SHEPHERD_AGENT_ID can't
            // change after spawn anyway.
            if register_handshake(&mut stream, &mut reader, &self.register_params).is_err() {
                continue;
            }
            *self
                .writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = stream;
            self.broken.store(false, Ordering::Release);
            std::thread::spawn(move || drain_responses(reader));
            for method in self.replay_methods() {
                self.notify(method);
            }
            return;
        }
    }
}

fn cache(slot: &Mutex<Option<Method>>, method: &Method) {
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(method.clone());
}

fn next_backoff(delay: Duration) -> Duration {
    (delay * 2).min(RECONNECT_CEILING)
}

/// Sends agent.register and reads the response. Used at launch and on every
/// reconnect; `params.agent_id` carries the retained id when present.
fn register_handshake(
    stream: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
    params: &crate::protocol::AgentRegisterParams,
) -> std::io::Result<String> {
    let request = Request {
        id: Some(serde_json::json!(1)),
        method: Method::AgentRegister(params.clone()),
    };
    let mut line = serde_json::to_string(&request).expect("request should serialize");
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut response_line = String::new();
    reader.read_line(&mut response_line)?;
    if response_line.is_empty() {
        return Err(std::io::Error::other("server closed during registration"));
    }
    let response: Response = serde_json::from_str(&response_line)
        .map_err(|err| std::io::Error::other(format!("bad register response: {err}")))?;
    response
        .result
        .as_ref()
        .and_then(|result| result.get("agent_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| std::io::Error::other("server did not return an agent_id"))
}

/// Drains server responses so the server never blocks writing to us; exits
/// on EOF when its connection dies.
fn drain_responses(mut reader: BufReader<UnixStream>) {
    let mut sink = String::new();
    while let Ok(read) = reader.read_line(&mut sink) {
        if read == 0 {
            break;
        }
        sink.clear();
    }
}

pub fn run(name: Option<String>, argv: Vec<String>) -> std::io::Result<i32> {
    if argv.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no command given",
        ));
    }
    let agent = identify_agent_from_argv(&argv);
    let agent_label = agent.map(|agent| crate::detect::agent_label(agent).to_string());

    // Connect (auto-starting the server if needed — launch only; the
    // reconnect path is retry-only) and register.
    let mut stream = connect_or_start_server()?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let mut register_params = crate::protocol::AgentRegisterParams {
        agent_id: None,
        name,
        agent: agent_label.clone(),
        argv: argv.clone(),
        cwd,
        pid: std::process::id(),
        terminal: detect_terminal_location(|var| std::env::var(var).ok()),
    };
    let agent_id = register_handshake(&mut stream, &mut reader, &register_params)?;
    // Future re-registrations must reclaim this exact id: it's exported to
    // the child as SHEPHERD_AGENT_ID and hooks report with it for life.
    register_params.agent_id = Some(agent_id.clone());
    let client = Arc::new(IngestClient {
        writer: Mutex::new(stream),
        broken: AtomicBool::new(false),
        register_params,
        last_rename: Mutex::new(None),
        last_detection: Mutex::new(None),
        last_session: Mutex::new(None),
    });

    // Drain further responses so the server never blocks writing to us.
    std::thread::spawn(move || drain_responses(reader));

    // Spawn the agent under a PTY sized to the real terminal.
    let (rows, cols) = terminal_size();
    let pty = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(std::io::Error::other)?;
    let mut command = CommandBuilder::new(&argv[0]);
    command.args(&argv[1..]);
    if let Ok(dir) = std::env::current_dir() {
        command.cwd(dir);
    }
    command.env("SHEPHERD_ENV", "1");
    command.env("SHEPHERD_SOCKET_PATH", socket_path());
    command.env("SHEPHERD_AGENT_ID", &agent_id);
    let mut child = pty
        .slave
        .spawn_command(command)
        .map_err(std::io::Error::other)?;
    drop(pty.slave);
    let master = pty.master;

    let emulation = Arc::new(Mutex::new(Emulation {
        parser: vt100::Parser::new(rows, cols, VT_SCROLLBACK_LINES),
        osc: crate::osc::AgentOscStateTracker::default(),
    }));
    let detection_content_seq = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    // Raw mode for transparent passthrough; restored on drop.
    let raw_guard = RawModeGuard::new();

    // PTY → stdout + emulation.
    let mut pty_reader = master.try_clone_reader().map_err(std::io::Error::other)?;
    {
        let emulation = emulation.clone();
        let detection_content_seq = detection_content_seq.clone();
        std::thread::spawn(move || {
            let mut stdout = std::io::stdout();
            let mut buf = [0u8; 8192];
            loop {
                match pty_reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let bytes = &buf[..read];
                        if stdout
                            .write_all(bytes)
                            .and_then(|()| stdout.flush())
                            .is_err()
                        {
                            break;
                        }
                        {
                            let mut emulation = emulation
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            emulation.parser.process(bytes);
                            emulation.osc.observe(bytes);
                        }
                        detection_content_seq.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });
    }

    // stdin → PTY, with Seen evidence reports.
    let mut pty_writer = master.take_writer().map_err(std::io::Error::other)?;
    {
        let client = client.clone();
        let agent_id = agent_id.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 4096];
            let mut last_seen_report: Option<Instant> = None;
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if pty_writer.write_all(&buf[..read]).is_err() {
                            break;
                        }
                        let _ = pty_writer.flush();
                        let due =
                            last_seen_report.is_none_or(|at| at.elapsed() >= SEEN_REPORT_THROTTLE);
                        if due {
                            last_seen_report = Some(Instant::now());
                            client.notify(Method::AgentSeen(crate::protocol::AgentTarget {
                                agent_id: agent_id.clone(),
                            }));
                        }
                    }
                }
            }
        });
    }

    // SIGWINCH → resize PTY and emulation. The thread takes ownership of
    // the PTY master, keeping it alive for the process lifetime.
    {
        let emulation = emulation.clone();
        let mut signals = signal_hook::iterator::Signals::new([libc::SIGWINCH])?;
        std::thread::spawn(move || {
            for _ in signals.forever() {
                let (rows, cols) = terminal_size();
                let _ = master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
                emulation
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .parser
                    .set_size(rows, cols);
            }
        });
    }

    // Claude /rename propagation: poll Claude's session metadata file and
    // push name changes as agent.rename.
    if agent == Some(Agent::Claude) {
        if let Some(child_pid) = child.process_id() {
            let client = client.clone();
            let agent_id = agent_id.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                poll_claude_session_name(child_pid, &agent_id, &client, &stop);
            });
        }
    }

    // Keepalive + reconnect: the periodic ping doubles as the liveness
    // probe (its failed write flips `broken`, even for idle sessions);
    // this thread then reconnects, re-registers, and replays state.
    {
        let client = client.clone();
        let stop = stop.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(PING_INTERVAL);
            if stop.load(Ordering::Acquire) {
                return;
            }
            if !client.broken.load(Ordering::Acquire) {
                client.notify(Method::Ping(crate::protocol::EmptyParams {}));
            }
            if client.broken.load(Ordering::Acquire) {
                client.reconnect(&stop);
            }
        });
    }

    // Detection loop.
    {
        let emulation = emulation.clone();
        let client = client.clone();
        let agent_id = agent_id.clone();
        let stop = stop.clone();
        let detection_content_seq = detection_content_seq.clone();
        std::thread::spawn(move || {
            detection_loop(
                agent,
                &agent_id,
                &client,
                &emulation,
                &detection_content_seq,
                &stop,
            );
        });
    }

    let status = child.wait().map_err(std::io::Error::other)?;
    stop.store(true, Ordering::Release);
    drop(raw_guard);
    // Dropping the process ends the ingest connection, which removes the
    // agent server-side. Exit directly: the stdin thread blocks forever.
    Ok(status.exit_code() as i32)
}

fn detection_loop(
    agent: Option<Agent>,
    agent_id: &str,
    client: &IngestClient,
    emulation: &Mutex<Emulation>,
    detection_content_seq: &AtomicU64,
    stop: &AtomicBool,
) {
    let started = Instant::now();
    let mut state = AgentState::Idle;
    let mut last_visible_idle = true;
    let mut last_visible_blocker = false;
    let mut last_visible_working = false;
    let mut last_visible_signal_refresh: Option<Instant> = None;
    let mut last_screen_scan_seq: Option<u64> = None;
    let mut pending_idle = PendingIdleConfirmation::default();
    let mut last_activity: Option<String> = None;

    loop {
        let interval = if pending_idle.active() {
            AGENT_PENDING_IDLE_RECHECK
        } else {
            DETECTION_INTERVAL
        };
        std::thread::sleep(interval);
        if stop.load(Ordering::Acquire) {
            return;
        }
        let now = Instant::now();
        if now.duration_since(started) < AGENT_STARTUP_GRACE_WINDOW {
            continue;
        }

        let current_seq = Some(detection_content_seq.load(Ordering::Relaxed));
        match decide_detection_screen_read(DetectionScreenReadInput {
            state,
            agent,
            pending_idle_active: pending_idle.active(),
            agent_changed: false,
            process_exited: false,
            current_detection_content_seq: current_seq,
            last_screen_scan_detection_content_seq: last_screen_scan_seq,
        }) {
            DetectionScreenReadDecision::Read => {}
            DetectionScreenReadDecision::Skip => continue,
        }

        let (content, osc_title, osc_progress) = {
            let emulation = emulation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                emulation.parser.screen().contents(),
                emulation.osc.latest_title().to_string(),
                emulation.osc.latest_progress().to_string(),
            )
        };
        last_screen_scan_seq = current_seq;

        // The agent's own line about its work. Published independently of the
        // state decision below: a title can change many times within one state
        // (working -> working), and state publishes are deliberately deduped.
        let activity = activity_from_title(&osc_title);
        if activity != last_activity {
            last_activity = activity.clone();
            client.notify(Method::AgentReportActivity(
                crate::protocol::AgentReportActivityParams {
                    agent_id: agent_id.to_string(),
                    activity,
                },
            ));
        }

        if crate::detect::should_skip_state_update(agent, &content) {
            pending_idle.clear();
            continue;
        }

        let Some(detection) = detection_update_for_publish_with_osc(
            agent,
            &content,
            &osc_title,
            &osc_progress,
            false,
        ) else {
            continue;
        };

        let decision = decide_screen_detection_publish(
            ScreenDetectionPublishInput {
                current_state: state,
                last_visible_idle,
                last_visible_blocker,
                last_visible_working,
                last_visible_signal_refresh,
                screen_detection: detection,
                process_exited: false,
                agent_changed: false,
                now,
            },
            &mut pending_idle,
        );

        if let DetectionPublishDecision::Publish {
            state: new_state,
            visible_idle,
            visible_blocker,
            visible_working,
            process_exited,
        } = decision
        {
            state = new_state;
            last_visible_idle = visible_idle;
            last_visible_blocker = visible_blocker;
            last_visible_working = visible_working;
            if visible_blocker {
                last_visible_signal_refresh = Some(now);
            }
            client.notify(Method::AgentReportDetection(
                crate::protocol::AgentReportDetectionParams {
                    agent_id: agent_id.to_string(),
                    agent: agent.map(|agent| crate::detect::agent_label(agent).to_string()),
                    state: new_state.label().to_string(),
                    visible_blocker,
                    visible_working,
                    process_exited,
                },
            ));
        }
    }
}

/// Watches `~/.claude/sessions/<pid>.json` for the `name` field that
/// Claude Code's /rename writes, and propagates changes as agent.rename.
/// The file is keyed by Claude's own pid — ours, since the wrapper spawns
/// claude directly. ponytail: if claude sits behind a shim (pid mismatch)
/// the file never appears and this silently does nothing; match by
/// sessionId if that ever matters.
fn poll_claude_session_name(
    child_pid: u32,
    agent_id: &str,
    client: &IngestClient,
    stop: &AtomicBool,
) {
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    let path = std::path::Path::new(&home)
        .join(".claude")
        .join("sessions")
        .join(format!("{child_pid}.json"));
    let mut last_mtime: Option<std::time::SystemTime> = None;
    let mut last_name: Option<String> = None;
    let mut last_session_id: Option<String> = None;
    loop {
        std::thread::sleep(CLAUDE_SESSION_POLL);
        if stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let mtime = metadata.modified().ok();
        if mtime == last_mtime {
            continue;
        }
        last_mtime = mtime;

        // Identity is reported independently of the name: the two change at
        // different times, and a session that never gets renamed still has
        // an id worth reporting. Whatever the file currently says wins, so a
        // session that started a new conversation reports the new id.
        let session_id = claude_session_id(&path);
        if session_id.is_some() && session_id != last_session_id {
            last_session_id = session_id.clone();
            client.notify(Method::AgentReportSession(
                crate::protocol::AgentReportSessionParams {
                    agent_id: agent_id.to_string(),
                    source: "shepherd:claude".to_string(),
                    agent: "claude".to_string(),
                    seq: None,
                    agent_session_id: session_id,
                    agent_session_path: None,
                },
            ));
        }

        let Some(name) = claude_session_name(&path) else {
            continue;
        };
        if last_name.as_deref() == Some(name.as_str()) {
            continue;
        }
        last_name = Some(name.clone());
        client.notify(Method::AgentRename(crate::protocol::AgentRenameParams {
            agent_id: agent_id.to_string(),
            name,
        }));
    }
}

/// Reads the `name` field (set by /rename) from Claude Code's session
/// metadata file. Undocumented internal format — fail silent on any change.
/// The agent's resumable session id, from the same file the rename poller
/// reads. Attribution is structural: the file is keyed by the pid of the
/// child this wrapper spawned, so it cannot describe another session.
fn claude_session_id(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let id = json.get("sessionId")?.as_str()?.trim();
    (!id.is_empty()).then(|| id.to_string())
}

fn claude_session_name(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let name = json.get("name")?.as_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Terminal Location: which terminal this wrapper runs in, from env vars.
/// Inside tmux the inherited ITERM_SESSION_ID may describe where the tmux
/// *server* was born, not the attached tab — report the truthful innermost
/// layer (the tmux pane) instead.
fn detect_terminal_location(
    env: impl Fn(&str) -> Option<String>,
) -> Option<crate::protocol::TerminalLocation> {
    if env("TMUX").is_some() {
        let pane = env("TMUX_PANE").filter(|pane| !pane.is_empty())?;
        return Some(crate::protocol::TerminalLocation {
            app: "tmux".to_string(),
            session_id: pane,
        });
    }
    let iterm = env("ITERM_SESSION_ID")?;
    // Format: "w0t4p0:UUID" — keep only the stable UUID; the positional
    // prefix is a spawn-time snapshot that goes stale on tab reorder.
    let session_id = iterm.rsplit(':').next().filter(|id| !id.is_empty())?;
    Some(crate::protocol::TerminalLocation {
        app: "iTerm2".to_string(),
        session_id: session_id.to_string(),
    })
}

fn identify_agent_from_argv(argv: &[String]) -> Option<Agent> {
    let basename = argv[0]
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .unwrap_or(&argv[0]);
    parse_agent_label(basename)
}

fn connect_or_start_server() -> std::io::Result<UnixStream> {
    let path = socket_path();
    if let Ok(stream) = UnixStream::connect(&path) {
        return Ok(stream);
    }

    // No server: start one detached, logging beside the socket.
    let log_path = path.with_file_name("server.log");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .process_group(0)
        .spawn()?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(&path) {
            Ok(stream) => return Ok(stream),
            Err(err) if Instant::now() >= deadline => {
                return Err(std::io::Error::other(format!(
                    "could not reach shep server at {} after starting it: {err}",
                    path.display()
                )));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn terminal_size() -> (u16, u16) {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
    if ok && size.ws_row > 0 && size.ws_col > 0 {
        (size.ws_row, size.ws_col)
    } else {
        (24, 80)
    }
}

/// Puts stdin into raw mode for transparent passthrough; restores the
/// original settings on drop (including panic unwinds).
struct RawModeGuard {
    original: Option<libc::termios>,
}

impl RawModeGuard {
    fn new() -> Self {
        let fd = libc::STDIN_FILENO;
        if unsafe { libc::isatty(fd) } != 1 {
            return Self { original: None };
        }
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            return Self { original: None };
        }
        let original = unsafe { original.assume_init() };
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Self { original: None };
        }
        Self {
            original: Some(original),
        }
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if let Some(original) = &self.original {
            unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, original) };
        }
    }
}


/// Maximum characters carried for an agent's activity line; surfaces truncate
/// further to fit.
const MAX_ACTIVITY_CHARS: usize = 160;

/// Leading decoration agents put before their title text: spinner frames
/// (braille, half-circles) and idle/bullet markers. These say the same thing
/// the status already says, so they are stripped rather than displayed.
fn is_title_decoration(ch: char) -> bool {
    matches!(ch,
        '\u{2800}'..='\u{28FF}'   // braille spinner frames
        | '\u{25D0}'..='\u{25D3}' // half-circle spinner frames
        | '\u{2733}'              // ✳ idle marker
        | '\u{2736}' | '\u{273B}' | '\u{273D}' | '\u{2722}'
        | '\u{00B7}' | '\u{002A}' // · *
        | '\u{23F5}' | '\u{23F8}' // ⏵ ⏸
    )
}

/// Turn a raw OSC title into the line shown to the user, or None when it
/// carries nothing meaningful. The OSC tracker has already stripped control
/// characters; this removes leading decoration and caps the length.
fn activity_from_title(title: &str) -> Option<String> {
    let text = title
        .trim_start_matches(|ch: char| is_title_decoration(ch) || ch.is_whitespace())
        .trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(MAX_ACTIVITY_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::{activity_from_title, MAX_ACTIVITY_CHARS};

    #[test]
    fn activity_strips_spinner_and_idle_decoration() {
        assert_eq!(
            activity_from_title("◐ refactoring the parser").as_deref(),
            Some("refactoring the parser")
        );
        assert_eq!(
            activity_from_title("⠧ thinking").as_deref(),
            Some("thinking")
        );
        assert_eq!(
            activity_from_title("✳ Fixed the login timeout").as_deref(),
            Some("Fixed the login timeout")
        );
    }

    #[test]
    fn activity_is_none_when_nothing_meaningful_remains() {
        assert_eq!(activity_from_title(""), None);
        assert_eq!(activity_from_title("   "), None);
        // Decoration only: the status already says this; showing it would be noise.
        assert_eq!(activity_from_title("◐"), None);
        assert_eq!(activity_from_title("✳  "), None);
    }

    #[test]
    fn activity_keeps_ordinary_titles_and_caps_length() {
        assert_eq!(
            activity_from_title("shepherd — main").as_deref(),
            Some("shepherd — main")
        );
        let long = activity_from_title(&format!("◐ {}", "x".repeat(500)))
            .expect("a long title should still produce a line");
        assert_eq!(long.chars().count(), MAX_ACTIVITY_CHARS);
    }

    use super::detect_terminal_location;
    use super::{next_backoff, IngestClient, RECONNECT_CEILING, RECONNECT_INITIAL};
    use crate::protocol::Method;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    fn test_client(stream: UnixStream) -> IngestClient {
        IngestClient {
            writer: Mutex::new(stream),
            broken: AtomicBool::new(false),
            register_params: crate::protocol::AgentRegisterParams {
                agent_id: Some("agent_1".to_string()),
                name: None,
                agent: None,
                argv: vec!["sh".to_string()],
                cwd: "/tmp".to_string(),
                pid: 1,
                terminal: None,
            },
            last_rename: Mutex::new(None),
            last_detection: Mutex::new(None),
            last_session: Mutex::new(None),
        }
    }

    #[test]
    fn backoff_doubles_and_caps_at_the_ceiling() {
        let mut delay = RECONNECT_INITIAL;
        let mut schedule = Vec::new();
        for _ in 0..8 {
            delay = next_backoff(delay);
            schedule.push(delay);
        }
        assert_eq!(schedule[0], Duration::from_millis(200));
        assert_eq!(schedule[1], Duration::from_millis(400));
        assert!(schedule.iter().all(|delay| *delay <= RECONNECT_CEILING));
        assert_eq!(
            *schedule.last().expect("schedule is non-empty"),
            RECONNECT_CEILING
        );
    }

    #[test]
    fn notify_caches_only_the_latest_rename_and_detection() {
        let (ours, _peer) = UnixStream::pair().expect("socketpair should open");
        let client = test_client(ours);
        assert!(client.replay_methods().is_empty());

        client.notify(Method::AgentSeen(crate::protocol::AgentTarget {
            agent_id: "agent_1".to_string(),
        }));
        assert!(
            client.replay_methods().is_empty(),
            "seen must not be replayed"
        );

        client.notify(Method::AgentRename(crate::protocol::AgentRenameParams {
            agent_id: "agent_1".to_string(),
            name: "old".to_string(),
        }));
        client.notify(Method::AgentRename(crate::protocol::AgentRenameParams {
            agent_id: "agent_1".to_string(),
            name: "new".to_string(),
        }));
        client.notify(Method::AgentReportDetection(
            crate::protocol::AgentReportDetectionParams {
                agent_id: "agent_1".to_string(),
                agent: None,
                state: "working".to_string(),
                visible_blocker: false,
                visible_working: true,
                process_exited: false,
            },
        ));

        let replay = client.replay_methods();
        assert_eq!(replay.len(), 2);
        match &replay[0] {
            Method::AgentRename(params) => assert_eq!(params.name, "new"),
            other => panic!("expected rename first, got {other:?}"),
        }
        assert!(matches!(replay[1], Method::AgentReportDetection(_)));
        assert!(!client.broken.load(Ordering::Acquire));
    }

    #[test]
    fn failed_write_flags_the_connection_broken() {
        let (ours, peer) = UnixStream::pair().expect("socketpair should open");
        let client = test_client(ours);
        drop(peer);
        // The first write after peer death may land in a buffer; the write
        // path must flag `broken` once the failure surfaces.
        for _ in 0..3 {
            client.notify(Method::Ping(crate::protocol::EmptyParams {}));
        }
        assert!(client.broken.load(Ordering::Acquire));
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        move |var| {
            pairs
                .iter()
                .find(|(name, _)| name == var)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn iterm_session_id_yields_uuid_only() {
        let location = detect_terminal_location(env(&[(
            "ITERM_SESSION_ID",
            "w0t4p0:1B0DF43A-DAA4-4C55-A299-4F0B6C3C1DAA",
        )]))
        .expect("iTerm2 should be detected");
        assert_eq!(location.app, "iTerm2");
        assert_eq!(location.session_id, "1B0DF43A-DAA4-4C55-A299-4F0B6C3C1DAA");
    }

    #[test]
    fn tmux_wins_over_inherited_iterm_id() {
        let location = detect_terminal_location(env(&[
            ("TMUX", "/tmp/tmux-501/default,123,0"),
            ("TMUX_PANE", "%5"),
            ("ITERM_SESSION_ID", "w0t4p0:STALE"),
        ]))
        .expect("tmux should be detected");
        assert_eq!(location.app, "tmux");
        assert_eq!(location.session_id, "%5");
    }

    #[test]
    fn replay_restores_session_identity_after_reconnect() {
        let (ours, _theirs) = UnixStream::pair().expect("socket pair should be created");
        let client = test_client(ours);
        client.notify(Method::AgentReportSession(
            crate::protocol::AgentReportSessionParams {
                agent_id: "agent_1".to_string(),
                source: "shepherd:claude".to_string(),
                agent: "claude".to_string(),
                seq: None,
                agent_session_id: Some("ses-1".to_string()),
                agent_session_path: None,
            },
        ));

        // What a fresh server would be told on re-registration.
        let replayed: Vec<_> = client
            .replay_methods()
            .into_iter()
            .filter_map(|method| match method {
                Method::AgentReportSession(params) => params.agent_session_id,
                _ => None,
            })
            .collect();
        assert_eq!(replayed, vec!["ses-1".to_string()]);
    }

    #[test]
    fn claude_session_id_reads_session_field() {
        let dir = std::env::temp_dir().join(format!("shep-claude-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir should be created");
        let path = dir.join("321.json");

        // Missing file, malformed JSON and an absent/blank id all yield None
        // rather than a guess.
        assert_eq!(super::claude_session_id(&path), None);
        std::fs::write(&path, "not json").expect("file should write");
        assert_eq!(super::claude_session_id(&path), None);
        std::fs::write(&path, r#"{"pid":321,"name":"x"}"#).expect("file should write");
        assert_eq!(super::claude_session_id(&path), None);
        std::fs::write(&path, r#"{"pid":321,"sessionId":"  "}"#).expect("file should write");
        assert_eq!(super::claude_session_id(&path), None);

        std::fs::write(
            &path,
            r#"{"pid":321,"sessionId":"96d80aea-399d-45e5-8ef1-55031762757b","name":"shep"}"#,
        )
        .expect("file should write");
        assert_eq!(
            super::claude_session_id(&path).as_deref(),
            Some("96d80aea-399d-45e5-8ef1-55031762757b")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_session_name_reads_rename_field() {
        let dir = std::env::temp_dir().join(format!("shep-claude-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir should be created");
        let path = dir.join("123.json");

        std::fs::write(&path, r#"{"pid":123,"sessionId":"s","status":"idle"}"#)
            .expect("file should write");
        assert_eq!(super::claude_session_name(&path), None);

        std::fs::write(
            &path,
            r#"{"pid":123,"sessionId":"s","name":"fix auth bug","status":"idle"}"#,
        )
        .expect("file should write");
        assert_eq!(
            super::claude_session_name(&path).as_deref(),
            Some("fix auth bug")
        );

        std::fs::write(&path, "not json").expect("file should write");
        assert_eq!(super::claude_session_name(&path), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_terminal_yields_none() {
        assert_eq!(detect_terminal_location(env(&[])), None);
        // tmux without a pane id: omit rather than guess.
        assert_eq!(
            detect_terminal_location(env(&[
                ("TMUX", "/tmp/tmux"),
                ("ITERM_SESSION_ID", "w0t0p0:X")
            ])),
            None
        );
    }
}
