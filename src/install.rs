//! Installing agent integrations: the Claude Code hook + shep-meta skill, and
//! the opencode plugins. One module so adding an agent does not grow an
//! unrelated one.

use std::io;
use std::path::{Path, PathBuf};

/// Embedded opencode plugins. The state plugin auto-loads from the plugins
/// directory; the TUI plugin needs an entry in tui.jsonc (see ensure_tui_plugin).
const OPENCODE_STATE_PLUGIN: &str = include_str!("assets/opencode/agent-state.js");
const OPENCODE_TUI_PLUGIN: &str = include_str!("assets/opencode/tui-session.js");
const OPENCODE_STATE_PLUGIN_NAME: &str = "shep-agent-state.js";
const OPENCODE_TUI_PLUGIN_NAME: &str = "shep-tui-session.js";
const OPENCODE_TUI_PLUGIN_SPEC: &str = "./shep-tui-session.js";

pub const SUPPORTED_AGENTS: [&str; 2] = ["claude", "opencode"];

/// Install one agent's integration. Returns the lines to report to the user.
pub fn install(agent: &str) -> io::Result<Vec<String>> {
    match agent {
        "claude" => {
            let settings = install_claude_hook()?;
            Ok(vec![
                format!("installed SessionStart hook into {}", settings.display()),
                "installed shep-meta skill into ~/.claude/skills/shep-meta/".to_string(),
            ])
        }
        "opencode" => install_opencode(),
        other => Err(io::Error::other(format!(
            "unknown agent: {other} (supported: {})",
            SUPPORTED_AGENTS.join(", ")
        ))),
    }
}

// ---------------------------------------------------------------------------
// opencode
// ---------------------------------------------------------------------------

fn opencode_config_dir() -> io::Result<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Ok(Path::new(&xdg).join("opencode"));
        }
    }
    let home = std::env::var("HOME").map_err(|_| io::Error::other("HOME is not set"))?;
    Ok(Path::new(&home).join(".config").join("opencode"))
}

/// Write both plugins, then try to register the TUI one. A refused config edit
/// is reported but does not fail the install: state reporting is the part that
/// matters and it needs no config entry.
pub fn install_opencode() -> io::Result<Vec<String>> {
    let dir = opencode_config_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "opencode config directory not found at {}; install opencode first",
            dir.display()
        )));
    }

    let plugins_dir = dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir)?;
    let state_path = plugins_dir.join(OPENCODE_STATE_PLUGIN_NAME);
    std::fs::write(&state_path, OPENCODE_STATE_PLUGIN)?;
    let tui_path = dir.join(OPENCODE_TUI_PLUGIN_NAME);
    std::fs::write(&tui_path, OPENCODE_TUI_PLUGIN)?;

    let mut messages = vec![
        format!("installed opencode state plugin to {}", state_path.display()),
        format!("installed opencode tui plugin to {}", tui_path.display()),
    ];
    messages.push(ensure_tui_plugin(&dir)?);
    Ok(messages)
}

/// Register the TUI plugin in tui.jsonc without a JSONC parser. The config is
/// the user's, so we only make edits we can make exactly; anything else is
/// refused with the line to add (see specs 007 research D9).
fn ensure_tui_plugin(dir: &Path) -> io::Result<String> {
    let config_path = dir.join("tui.jsonc");
    let existing = match std::fs::read_to_string(&config_path) {
        Ok(existing) => existing,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            std::fs::write(
                &config_path,
                format!("{{\n  \"plugin\": [\"{OPENCODE_TUI_PLUGIN_SPEC}\"]\n}}\n"),
            )?;
            return Ok(format!("wrote {}", config_path.display()));
        }
        Err(err) => return Err(err),
    };

    match tui_config_with_plugin(&existing, OPENCODE_TUI_PLUGIN_SPEC) {
        TuiEdit::AlreadyPresent => Ok(format!(
            "tui plugin already registered in {}",
            config_path.display()
        )),
        TuiEdit::Updated(updated) => {
            std::fs::write(&config_path, updated)?;
            Ok(format!("registered tui plugin in {}", config_path.display()))
        }
        TuiEdit::Refused => Ok(format!(
            "NOTE: {} already has a \"plugin\" list, so it was left untouched.\n      \
             Add this entry yourself to enable tui session tracking: \"{}\"",
            config_path.display(),
            OPENCODE_TUI_PLUGIN_SPEC
        )),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TuiEdit {
    AlreadyPresent,
    Updated(String),
    Refused,
}

/// Pure: decide how (or whether) to add the plugin entry to a tui.jsonc body.
/// Inserting after the opening brace leaves every other byte — comments
/// included — exactly as the user wrote them.
fn tui_config_with_plugin(existing: &str, spec: &str) -> TuiEdit {
    if existing.contains(spec) {
        return TuiEdit::AlreadyPresent;
    }
    if existing.contains("\"plugin\"") {
        return TuiEdit::Refused;
    }
    let Some(brace) = existing.find('{') else {
        return TuiEdit::Refused;
    };
    let (head, tail) = existing.split_at(brace + 1);
    TuiEdit::Updated(format!("{head}\n  \"plugin\": [\"{spec}\"],{tail}"))
}


/// Instructions installed as a Claude Code skill so a supervised Claude can
/// tag its own session; the wrapper-injected SHEPHERD_AGENT_ID targets it.
const SHEP_META_SKILL: &str = r#"---
name: shep-meta
description: Tag the current shepherd-supervised session with metadata. Use when the user asks to tag, label, or describe this session, link it to a ticket (jira), or set/remove session metadata.
---

# shep-meta

Set metadata on the current supervised session:

    shep meta key=value ...

- Well-known keys: `jira` (ticket key, e.g. PROJ-123), `description` (short summary), `url`.
- Quote values with spaces: `shep meta description="Fixing login timeout"`.
- `key=` (empty value) removes a key; `shep meta` alone prints current metadata.
- Do not pass an agent name — the environment identifies the session.

If the command fails (for example, this session is not supervised by shepherd), report the error in one line and continue with the conversation — never retry or block on it.
"#;

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

    // The shep-meta skill rides along with the hook install.
    let skill_dir = claude_dir.join("skills").join("shep-meta");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(skill_dir.join("SKILL.md"), SHEP_META_SKILL)?;

    Ok(settings_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_config_is_created_case_by_case() {
        // Already registered: untouched.
        assert_eq!(
            tui_config_with_plugin("{\"plugin\":[\"./shep-tui-session.js\"]}", OPENCODE_TUI_PLUGIN_SPEC),
            TuiEdit::AlreadyPresent
        );
        // Someone else's plugin list: refuse rather than merge blindly.
        assert_eq!(
            tui_config_with_plugin("{\"plugin\":[\"other.js\"]}", OPENCODE_TUI_PLUGIN_SPEC),
            TuiEdit::Refused
        );
        // Not an object at all: refuse.
        assert_eq!(
            tui_config_with_plugin("[]", OPENCODE_TUI_PLUGIN_SPEC),
            TuiEdit::Refused
        );
    }

    #[test]
    fn insert_preserves_existing_comments_and_settings() {
        let existing = "{\n  // Keep this comment.\n  \"theme\": \"system\",\n}\n";
        let TuiEdit::Updated(updated) = tui_config_with_plugin(existing, OPENCODE_TUI_PLUGIN_SPEC)
        else {
            panic!("a config without a plugin list should be updated");
        };
        assert!(updated.contains("// Keep this comment."));
        assert!(updated.contains("\"theme\": \"system\""));
        assert!(updated.contains("\"plugin\": [\"./shep-tui-session.js\"]"));
        // Everything the user wrote survives verbatim.
        assert!(updated.ends_with("  // Keep this comment.\n  \"theme\": \"system\",\n}\n"));
    }

    #[test]
    fn unknown_agent_is_rejected() {
        let err = install("nope").expect_err("unknown agent should error");
        assert!(err.to_string().contains("claude"));
    }
}
