//! Parser for Nix's `--log-format internal-json` event stream.
//!
//! Each line is `@nix {json}`. We only keep what diagnosis needs:
//! errors (level 0 `msg` events, with their structured trace), and per
//! build activity the derivation, the current phase and the log lines.

use crate::util::strip_ansi;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Activity types (src/libutil/logging.hh)
const ACT_BUILD: u64 = 105;
// Result types
const RES_BUILD_LOG_LINE: u64 = 101;
const RES_SET_PHASE: u64 = 104;

#[derive(Debug, Deserialize)]
struct RawEvent {
    action: String,
    #[serde(default)]
    id: u64,
    #[serde(default)]
    level: Option<u64>,
    #[serde(rename = "type", default)]
    ty: Option<u64>,
    #[serde(default)]
    fields: Vec<serde_json::Value>,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    raw_msg: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    column: Option<u64>,
    #[serde(default)]
    trace: Vec<RawFrame>,
}

#[derive(Debug, Deserialize)]
struct RawFrame {
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    column: Option<u64>,
    #[serde(default)]
    raw_msg: Option<String>,
}

/// A source position as Nix reports it. `file` is a real path, or a
/// pseudo-file like `«string»`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Pos {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Frame {
    pub pos: Option<Pos>,
    pub msg: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NixError {
    /// The error message itself, ANSI stripped, without trace.
    pub message: String,
    /// Fully formatted message as Nix would print it (with ANSI codes).
    #[serde(skip)]
    pub formatted: String,
    /// Position attached to the error itself (e.g. parse errors).
    pub pos: Option<Pos>,
    /// Trace frames, innermost first.
    pub trace: Vec<Frame>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BuildActivity {
    pub drv: String,
    pub phase: Option<String>,
    pub log: Vec<String>,
}

#[derive(Debug, Default)]
pub struct LogState {
    pub errors: Vec<NixError>,
    pub builds: HashMap<u64, BuildActivity>,
    /// Non-JSON stderr lines, kept for when Nix fails without a JSON error.
    pub plain: Vec<String>,
}

/// What a single event means for the live progress display.
pub enum Progress {
    None,
    Building(String),
    Phase(String, String),
    /// Warning / info message that should be shown to the user right away.
    Message(String),
}

fn mk_pos(file: Option<&str>, line: Option<u64>, column: Option<u64>) -> Option<Pos> {
    let file = file?;
    let line = line.unwrap_or(0) as usize;
    if line == 0 || file.starts_with("«none»") {
        return None;
    }
    // Nix puts `path:line:col` into `file` and also `line`/`column` fields.
    let suffix = format!(":{}:{}", line, column.unwrap_or(0));
    let file = file.strip_suffix(&suffix).unwrap_or(file);
    Some(Pos {
        file: file.to_string(),
        line,
        column: column.unwrap_or(1) as usize,
    })
}

impl LogState {
    pub fn feed_line(&mut self, line: &str) -> Progress {
        let Some(json) = line
            .strip_prefix("@nix ")
            .or_else(|| line.starts_with('{').then_some(line))
        else {
            if !line.trim().is_empty() {
                self.plain.push(strip_ansi(line));
            }
            return Progress::None;
        };
        let Ok(ev) = serde_json::from_str::<RawEvent>(json) else {
            return Progress::None;
        };
        match ev.action.as_str() {
            "msg" => {
                let level = ev.level.unwrap_or(9);
                // Nix logs some warnings (substituters, …) at error level.
                let text = strip_ansi(ev.raw_msg.as_deref().or(ev.msg.as_deref()).unwrap_or_default());
                let is_warning = text.trim_start().starts_with("warning:")
                    || text.trim_start().starts_with("error (ignored)")
                    || text.starts_with("disabling binary cache");
                if level == 0 && !is_warning {
                    let message = strip_ansi(ev.raw_msg.as_deref().unwrap_or_default()).trim().to_string();
                    let trace = ev
                        .trace
                        .iter()
                        .map(|f| Frame {
                            pos: mk_pos(f.file.as_deref(), f.line, f.column),
                            msg: strip_ansi(f.raw_msg.as_deref().unwrap_or_default()).trim().to_string(),
                        })
                        .collect();
                    let formatted = ev.msg.clone().unwrap_or_default();
                    let message = if message.is_empty() {
                        strip_ansi(&formatted)
                            .trim()
                            .trim_start_matches("error:")
                            .trim()
                            .to_string()
                    } else {
                        message
                    };
                    self.errors.push(NixError {
                        message,
                        formatted,
                        pos: mk_pos(ev.file.as_deref(), ev.line, ev.column),
                        trace,
                    });
                    Progress::None
                } else if level <= 1 {
                    Progress::Message(ev.msg.unwrap_or_default())
                } else {
                    Progress::None
                }
            }
            "start" if ev.ty == Some(ACT_BUILD) => {
                let drv = ev
                    .fields
                    .first()
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let name = crate::util::drv_name(&drv);
                self.builds.insert(
                    ev.id,
                    BuildActivity {
                        drv,
                        ..Default::default()
                    },
                );
                Progress::Building(name)
            }
            "result" => {
                let Some(b) = self.builds.get_mut(&ev.id) else {
                    return Progress::None;
                };
                let first = ev.fields.first().and_then(|v| v.as_str()).unwrap_or_default();
                match ev.ty {
                    Some(RES_BUILD_LOG_LINE) => {
                        if !first.starts_with("@nix ") {
                            b.log.push(strip_ansi(first));
                        }
                        Progress::None
                    }
                    Some(RES_SET_PHASE) => {
                        b.phase = Some(first.to_string());
                        Progress::Phase(crate::util::drv_name(&b.drv), first.to_string())
                    }
                    _ => Progress::None,
                }
            }
            _ => Progress::None,
        }
    }

    pub fn build_for(&self, drv: &str) -> Option<&BuildActivity> {
        self.builds.values().find(|b| b.drv == drv)
    }
}

pub fn parse_str(input: &str) -> LogState {
    let mut st = LogState::default();
    for line in input.lines() {
        st.feed_line(line);
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_build_failure() {
        let st = parse_str(include_str!("../tests/fixtures/build.jsonl"));
        let err = st
            .errors
            .iter()
            .find(|e| e.message.starts_with("Cannot build"))
            .unwrap();
        assert!(err.message.contains("exit code 3"));
        let b = st
            .build_for("/nix/store/1kzyzxzsgcmm8vsr6w3dbm3fdpbmx30x-failer.drv")
            .unwrap();
        assert_eq!(b.log, vec!["configuring", "fatal: thing missing"]);
    }

    #[test]
    fn parses_trace() {
        let st = parse_str(include_str!("../tests/fixtures/type.jsonl"));
        let err = &st.errors[0];
        assert_eq!(err.message, "expected a set but found a list: [ \"a\" ]");
        assert_eq!(err.trace[0].pos, None);
        assert_eq!(err.trace[1].pos.as_ref().unwrap().file, "«string»");
        assert_eq!(err.trace[1].pos.as_ref().unwrap().column, 41);
    }
}
