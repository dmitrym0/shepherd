//! The `shepherd run` wrapper: a transparent PTY shim. Spawns the agent under
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
const VT_SCROLLBACK_LINES: usize = 500;

/// Terminal emulation state fed by the PTY reader, read by the detection loop.
struct Emulation {
    parser: vt100::Parser,
    osc: crate::osc::AgentOscStateTracker,
}

struct IngestClient {
    writer: Mutex<UnixStream>,
}

impl IngestClient {
    /// Fire-and-forget notification; responses are drained by a separate
    /// thread. Errors are ignored — a dead server must never break the
    /// user's terminal session.
    fn notify(&self, method: Method) {
        let request = Request { id: None, method };
        let mut line = serde_json::to_string(&request).expect("request should serialize");
        line.push('\n');
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(line.as_bytes());
        }
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

    // Connect (auto-starting the server if needed) and register.
    let stream = connect_or_start_server()?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let client = Arc::new(IngestClient {
        writer: Mutex::new(stream),
    });
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let register = Request {
        id: Some(serde_json::json!(1)),
        method: Method::AgentRegister(crate::protocol::AgentRegisterParams {
            name,
            agent: agent_label.clone(),
            argv: argv.clone(),
            cwd,
            pid: std::process::id(),
        }),
    };
    {
        let mut line = serde_json::to_string(&register).expect("request should serialize");
        line.push('\n');
        let mut writer = client
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer.write_all(line.as_bytes())?;
    }
    let mut response_line = String::new();
    reader.read_line(&mut response_line)?;
    let response: Response = serde_json::from_str(&response_line)
        .map_err(|err| std::io::Error::other(format!("bad register response: {err}")))?;
    let agent_id = response
        .result
        .as_ref()
        .and_then(|result| result.get("agent_id"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| std::io::Error::other("server did not return an agent_id"))?
        .to_string();

    // Drain further responses so the server never blocks writing to us.
    std::thread::spawn(move || {
        let mut sink = String::new();
        while let Ok(read) = reader.read_line(&mut sink) {
            if read == 0 {
                break;
            }
            sink.clear();
        }
    });

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
    let mut child = pty.slave.spawn_command(command).map_err(std::io::Error::other)?;
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
                        if stdout.write_all(bytes).and_then(|()| stdout.flush()).is_err() {
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
                        let due = last_seen_report
                            .is_none_or(|at| at.elapsed() >= SEEN_REPORT_THROTTLE);
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
                    "could not reach shepherd server at {} after starting it: {err}",
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
