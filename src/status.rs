//! Local clients of the ingest socket: the `status` command, the Claude Code
//! hook entry point, and its installer.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::protocol::{
    now_epoch_ms, socket_path, AgentInfo, AgentStatus, Event, Method, Request, Response,
    EVENT_AGENT_REMOVED, EVENT_SNAPSHOT,
};

fn request(stream: &mut UnixStream, method: Method) -> std::io::Result<Response> {
    let request = Request {
        id: Some(serde_json::json!(1)),
        method,
    };
    let mut line = serde_json::to_string(&request).expect("request should serialize");
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut response_line = String::new();
    reader.read_line(&mut response_line)?;
    serde_json::from_str(&response_line)
        .map_err(|err| std::io::Error::other(format!("bad response: {err}")))
}

fn connect() -> std::io::Result<UnixStream> {
    let path = socket_path();
    UnixStream::connect(&path).map_err(|err| {
        std::io::Error::other(format!(
            "no shep server at {} ({err}); start an agent with `shep run` or run `shep serve`",
            path.display()
        ))
    })
}

pub fn status(watch: bool) -> std::io::Result<()> {
    let mut stream = connect()?;
    if !watch {
        let response = request(&mut stream, Method::AgentList(Default::default()))?;
        if let Some(error) = response.error {
            return Err(std::io::Error::other(error.message));
        }
        let agents: Vec<AgentInfo> = response
            .result
            .and_then(|result| result.get("agents").cloned())
            .map(|value| serde_json::from_value(value).unwrap_or_default())
            .unwrap_or_default();
        print!("{}", render_table(agents.iter()));
        return Ok(());
    }

    let response = request(&mut stream, Method::EventsSubscribe(Default::default()))?;
    if let Some(error) = response.error {
        return Err(std::io::Error::other(error.message));
    }
    let mut agents: BTreeMap<String, AgentInfo> = BTreeMap::new();
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
        let Ok(event) = serde_json::from_str::<Event>(&line) else {
            continue;
        };
        match event.event.as_str() {
            EVENT_SNAPSHOT => {
                agents.clear();
                if let Some(list) = event.data.get("agents") {
                    let list: Vec<AgentInfo> =
                        serde_json::from_value(list.clone()).unwrap_or_default();
                    for info in list {
                        agents.insert(info.agent_id.clone(), info);
                    }
                }
            }
            EVENT_AGENT_REMOVED => {
                if let Some(agent_id) = event.data.get("agent_id").and_then(|id| id.as_str()) {
                    agents.remove(agent_id);
                }
            }
            _ => {
                if let Ok(info) = serde_json::from_value::<AgentInfo>(event.data.clone()) {
                    agents.insert(info.agent_id.clone(), info);
                }
            }
        }
        // Clear screen, home cursor, re-render.
        print!("\x1b[2J\x1b[H{}", render_table(agents.values()));
        std::io::stdout().flush()?;
    }
    Ok(())
}

fn render_table<'a>(agents: impl Iterator<Item = &'a AgentInfo>) -> String {
    let mut rows: Vec<[String; 6]> = Vec::new();
    for info in agents {
        let status = info.agent_status;
        let name = info
            .name
            .clone()
            .or_else(|| info.title.clone())
            .unwrap_or_default();
        let agent = info
            .display_agent
            .clone()
            .or_else(|| info.agent.clone())
            .unwrap_or_else(|| "?".to_string());
        let note = info
            .blocked_reason
            .clone()
            .or_else(|| info.custom_status.clone())
            .unwrap_or_default();
        rows.push([
            format!(
                "{}{}\x1b[0m",
                status_color(status),
                status.label()
            ),
            agent,
            name,
            humanize_age(now_epoch_ms().saturating_sub(info.status_since_ms)),
            info.cwd.clone().unwrap_or_default(),
            note,
        ]);
    }
    if rows.is_empty() {
        return "no supervised agents\n".to_string();
    }
    let mut out = String::new();
    let header = ["STATUS", "AGENT", "NAME", "FOR", "CWD", "NOTE"];
    // Column widths ignore ANSI escapes; the status column is padded by its
    // visible label instead.
    let mut widths = header.map(str::len);
    for row in &rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(visible_len(cell));
        }
    }
    for (index, title) in header.iter().enumerate() {
        out.push_str(&pad(title, widths[index]));
        out.push_str("  ");
    }
    out.push('\n');
    for row in &rows {
        for (index, cell) in row.iter().enumerate() {
            out.push_str(&pad(cell, widths[index]));
            out.push_str("  ");
        }
        out.push('\n');
    }
    out
}

fn visible_len(text: &str) -> usize {
    let mut length = 0;
    let mut in_escape = false;
    for ch in text.chars() {
        if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else if ch == '\x1b' {
            in_escape = true;
        } else {
            length += 1;
        }
    }
    length
}

fn pad(text: &str, width: usize) -> String {
    let mut out = text.to_string();
    for _ in visible_len(text)..width {
        out.push(' ');
    }
    out
}

fn status_color(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "\x1b[32m",
        AgentStatus::Working => "\x1b[33m",
        AgentStatus::Blocked => "\x1b[31m",
        AgentStatus::Done => "\x1b[36m",
        AgentStatus::Unknown => "\x1b[2m",
    }
}

fn humanize_age(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

// ---------------------------------------------------------------------------
// Claude Code hook
// ---------------------------------------------------------------------------

/// Entry point invoked by Claude Code on SessionStart. Mirrors herdr's
/// claude integration v7: report the resumable session id, nothing else.
/// Must never fail loudly — a broken hook must not break Claude.
pub fn claude_hook(action: &str) -> std::io::Result<()> {
    if action != "session" {
        return Ok(());
    }
    if std::env::var("SHEPHERD_ENV").as_deref() != Ok("1") {
        return Ok(());
    }
    let Ok(agent_id) = std::env::var("SHEPHERD_AGENT_ID") else {
        return Ok(());
    };

    let mut input = String::new();
    std::io::stdin().take(1 << 20).read_to_string(&mut input)?;
    let hook_input: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();

    // Subagent events describe inner agents, not the supervised session.
    if hook_input.get("agent_id").is_some_and(|id| !id.is_null()) {
        return Ok(());
    }
    if hook_input.get("hook_event_name").and_then(|name| name.as_str()) == Some("SubagentStop") {
        return Ok(());
    }
    let Some(session_id) = hook_input
        .get("session_id")
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())
    else {
        return Ok(());
    };
    let transcript_path = hook_input
        .get("transcript_path")
        .and_then(|path| path.as_str())
        .filter(|path| !path.is_empty())
        .map(str::to_string);

    let seq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let mut stream = connect()?;
    let _ = request(
        &mut stream,
        Method::AgentReportSession(crate::protocol::AgentReportSessionParams {
            agent_id,
            source: "shepherd:claude".to_string(),
            agent: "claude".to_string(),
            seq: Some(seq),
            agent_session_id: Some(session_id.to_string()),
            agent_session_path: transcript_path,
        }),
    )?;
    Ok(())
}

/// Merge a SessionStart hook entry into ~/.claude/settings.json. Idempotent:
/// removes any previous shep claude-hook entries first.
pub fn install_claude_hook() -> std::io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| std::io::Error::other("HOME is not set"))?;
    let claude_dir = std::path::Path::new(&home).join(".claude");
    if !claude_dir.is_dir() {
        return Err(std::io::Error::other(format!(
            "claude directory not found at {}; install Claude Code first",
            claude_dir.display()
        )));
    }
    let settings_path = claude_dir.join("settings.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        serde_json::from_str(&std::fs::read_to_string(&settings_path)?).map_err(|err| {
            std::io::Error::other(format!("failed to parse {}: {err}", settings_path.display()))
        })?
    } else {
        serde_json::json!({})
    };

    let exe = std::env::current_exe()?;
    let command = format!("{} claude-hook session", exe.display());

    let hooks = settings
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("settings.json is not an object"))?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let session_start = hooks
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("settings.json hooks is not an object"))?
        .entry("SessionStart")
        .or_insert_with(|| serde_json::json!([]));
    let entries = session_start
        .as_array_mut()
        .ok_or_else(|| std::io::Error::other("SessionStart hooks is not an array"))?;

    // Drop previous shepherd entries (idempotent reinstall).
    for entry in entries.iter_mut() {
        if let Some(commands) = entry.get_mut("hooks").and_then(|hooks| hooks.as_array_mut()) {
            commands.retain(|command_entry| {
                !command_entry
                    .get("command")
                    .and_then(|command| command.as_str())
                    .is_some_and(|command| command.contains("claude-hook session"))
            });
        }
    }
    entries.retain(|entry| {
        entry
            .get("hooks")
            .and_then(|hooks| hooks.as_array())
            .is_none_or(|commands| !commands.is_empty())
    });

    entries.push(serde_json::json!({
        "matcher": "*",
        "hooks": [{ "type": "command", "command": command, "timeout": 10 }],
    }));

    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
    Ok(settings_path)
}
