//! Shepherd's first durable state: user metadata keyed by resumable session
//! id in `~/.shepherd/metadata.json`, plus optional server config in
//! `~/.shepherd/config.toml`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_KEYS_PER_SESSION: usize = 64;
pub const MAX_KEY_CHARS: usize = 64;
pub const MAX_VALUE_CHARS: usize = 500;
const MAX_SESSIONS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaValue {
    pub value: String,
    pub ts_ms: u64,
}

pub type SessionMetadata = HashMap<String, MetaValue>;

/// Per-key newest-write-wins merge of `from` into `into`. Ties go to `from`
/// so a fresh report beats an equally-old stored copy.
pub fn merge_newest(into: &mut SessionMetadata, from: &SessionMetadata) {
    for (key, value) in from {
        if into.get(key).is_none_or(|existing| existing.ts_ms <= value.ts_ms) {
            into.insert(key.clone(), value.clone());
        }
    }
}

pub struct MetadataStore {
    path: PathBuf,
    sessions: HashMap<String, SessionMetadata>,
}

impl MetadataStore {
    pub fn default_path() -> PathBuf {
        shepherd_dir().join("metadata.json")
    }

    /// A missing or unreadable file starts empty: metadata is never worth
    /// refusing to start the server over.
    pub fn load(path: PathBuf) -> Self {
        let sessions = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, sessions }
    }

    pub fn get(&self, session_key: &str) -> Option<&SessionMetadata> {
        self.sessions.get(session_key)
    }

    /// Replace one session's stored map and persist. The caller has already
    /// reconciled the map (merge_newest against the previous stored copy),
    /// so replacement is what makes deletions durable.
    pub fn replace(&mut self, session_key: &str, entries: SessionMetadata) {
        if entries.is_empty() {
            self.sessions.remove(session_key);
        } else {
            self.sessions.insert(session_key.to_string(), entries);
        }
        self.evict();
        if let Err(err) = self.save() {
            eprintln!("shepherd: failed to save {}: {err}", self.path.display());
        }
    }

    /// Keep the newest-touched MAX_SESSIONS sessions; a session's age is the
    /// newest ts_ms among its entries.
    fn evict(&mut self) {
        if self.sessions.len() <= MAX_SESSIONS {
            return;
        }
        let mut ages: Vec<(u64, String)> = self
            .sessions
            .iter()
            .map(|(key, entries)| {
                let newest = entries.values().map(|value| value.ts_ms).max().unwrap_or(0);
                (newest, key.clone())
            })
            .collect();
        ages.sort();
        for (_, key) in ages.iter().take(self.sessions.len() - MAX_SESSIONS) {
            self.sessions.remove(key);
        }
    }

    // ponytail: whole-file rewrite per change; per-session files if this
    // ever grows past ~1MB.
    fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_string(&self.sessions)?)?;
        std::fs::rename(&temp, &self.path)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jira_base_url: Option<String>,
}

impl Config {
    /// `~/.shepherd/config.toml`; missing or malformed → defaults.
    pub fn load() -> Self {
        std::fs::read_to_string(shepherd_dir().join("config.toml"))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }
}

fn shepherd_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".shepherd")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str, ts_ms: u64) -> MetaValue {
        MetaValue {
            value: text.to_string(),
            ts_ms,
        }
    }

    #[test]
    fn merge_newest_keeps_newer_and_ties_go_to_incoming() {
        let mut into: SessionMetadata = [
            ("a".to_string(), value("old", 1)),
            ("b".to_string(), value("newer", 9)),
            ("c".to_string(), value("tied", 5)),
        ]
        .into();
        let from: SessionMetadata = [
            ("a".to_string(), value("new", 2)),
            ("b".to_string(), value("older", 3)),
            ("c".to_string(), value("tie-wins", 5)),
        ]
        .into();
        merge_newest(&mut into, &from);
        assert_eq!(into["a"].value, "new");
        assert_eq!(into["b"].value, "newer");
        assert_eq!(into["c"].value, "tie-wins");
    }

    #[test]
    fn replace_persists_and_reloads() {
        let dir = std::env::temp_dir().join(format!("shep-store-test-{}", std::process::id()));
        let path = dir.join("metadata.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = MetadataStore::load(path.clone());
        store.replace(
            "session_id:s-1",
            [("jira".to_string(), value("PROJ-1", 10))].into(),
        );

        let reloaded = MetadataStore::load(path);
        assert_eq!(
            reloaded.get("session_id:s-1").and_then(|m| m.get("jira")),
            Some(&value("PROJ-1", 10))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_with_empty_map_removes_session() {
        let dir = std::env::temp_dir().join(format!("shep-store-empty-{}", std::process::id()));
        let path = dir.join("metadata.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = MetadataStore::load(path);
        store.replace("session_id:s-1", [("k".to_string(), value("v", 1))].into());
        store.replace("session_id:s-1", SessionMetadata::new());
        assert!(store.get("session_id:s-1").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn eviction_drops_oldest_sessions() {
        let dir = std::env::temp_dir().join(format!("shep-store-evict-{}", std::process::id()));
        let path = dir.join("metadata.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = MetadataStore::load(path);
        for index in 0..=MAX_SESSIONS as u64 {
            store.sessions.insert(
                format!("session_id:s-{index}"),
                [("k".to_string(), value("v", index))].into(),
            );
        }
        store.evict();
        assert_eq!(store.sessions.len(), MAX_SESSIONS);
        assert!(store.get("session_id:s-0").is_none(), "oldest should be evicted");
        assert!(store.get(&format!("session_id:s-{MAX_SESSIONS}")).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
