//! Passive OSC title/progress capture from raw PTY bytes.
//!
//! Copied from herdr's pane/osc.rs (https://github.com/ogulcancelik/herdr),
//! copyright Ogulcan Celik and contributors, AGPL-3.0-or-later.

/// Maximum retained string length for agent OSC title and progress payloads.
/// Title text is untrusted model output; cap it to bound memory.
const AGENT_OSC_MAX_CHARS: usize = 256;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum OscParseState {
    #[default]
    Ground,
    Escape,
    OscBody,
    OscEscape,
}

/// Always-on tracker that retains the latest OSC 0/2 title and OSC 9 progress
/// payload emitted by the child process. Pure passive capture for the
/// detection engine; nothing here affects rendering.
///
/// - `latest_title` — last OSC 0 or OSC 2 payload, sanitized. An empty
///   payload (e.g. `\x1b]0;\x07`) clears the stored value.
/// - `latest_progress` — last OSC 9 payload (the part after `9;`), stored
///   as-is after sanitization. E.g. `"4;3;"` or `"4;0;"`.
#[derive(Debug, Default)]
pub struct AgentOscStateTracker {
    state: OscParseState,
    body: Vec<u8>,
    latest_title: Option<String>,
    latest_progress: Option<String>,
}

impl AgentOscStateTracker {
    pub fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match self.state {
                OscParseState::Ground => {
                    if byte == 0x1b {
                        self.state = OscParseState::Escape;
                    }
                }
                OscParseState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = OscParseState::OscBody;
                    } else if byte == 0x1b {
                        self.state = OscParseState::Escape;
                    } else {
                        self.state = OscParseState::Ground;
                    }
                }
                OscParseState::OscBody => match byte {
                    0x07 => {
                        self.finalize();
                        self.state = OscParseState::Ground;
                    }
                    0x1b => self.state = OscParseState::OscEscape,
                    _ => self.body.push(byte),
                },
                OscParseState::OscEscape => {
                    if byte == b'\\' {
                        self.finalize();
                        self.state = OscParseState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = OscParseState::OscBody;
                    }
                }
            }

            if self.body.len() > 4096 {
                self.body.clear();
                self.state = OscParseState::Ground;
            }
        }
    }

    fn finalize(&mut self) {
        if let Some((command, payload)) = parse_agent_osc_body(&self.body) {
            match command {
                b"0" | b"2" => {
                    if payload.is_empty() {
                        self.latest_title = None;
                    } else {
                        self.latest_title =
                            Some(sanitize_agent_osc_string(payload, AGENT_OSC_MAX_CHARS));
                    }
                }
                b"9" => {
                    self.latest_progress =
                        Some(sanitize_agent_osc_string(payload, AGENT_OSC_MAX_CHARS));
                }
                _ => {}
            }
        }
        self.body.clear();
    }

    pub fn latest_title(&self) -> &str {
        self.latest_title.as_deref().unwrap_or("")
    }

    pub fn latest_progress(&self) -> &str {
        self.latest_progress.as_deref().unwrap_or("")
    }
}

/// Splits an OSC body at the first `;`, returning `(command, payload)`.
fn parse_agent_osc_body(body: &[u8]) -> Option<(&[u8], &[u8])> {
    let sep = body.iter().position(|&b| b == b';')?;
    Some((&body[..sep], &body[sep + 1..]))
}

fn sanitize_agent_osc_string(payload: &[u8], max_chars: usize) -> String {
    let text = String::from_utf8_lossy(payload);
    let mut out = String::new();
    for ch in text.chars().filter(|ch| !ch.is_control()).take(max_chars) {
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_title_and_progress() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe(b"\x1b]0;\xe2\xa0\xa7 thinking\x07");
        assert_eq!(tracker.latest_title(), "⠧ thinking");

        tracker.observe(b"\x1b]9;4;3;\x07");
        assert_eq!(tracker.latest_progress(), "4;3;");

        // ST terminator works too.
        tracker.observe(b"\x1b]2;done\x1b\\");
        assert_eq!(tracker.latest_title(), "done");

        // Empty title payload clears.
        tracker.observe(b"\x1b]0;\x07");
        assert_eq!(tracker.latest_title(), "");
    }

    #[test]
    fn split_writes_are_reassembled() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe(b"\x1b]0;par");
        tracker.observe(b"tial\x07");
        assert_eq!(tracker.latest_title(), "partial");
    }
}
