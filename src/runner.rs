//! Runs Nix with `--log-format internal-json` and keeps a compact progress
//! line on stderr. Costs nothing extra when the build succeeds.

use crate::nixlog::{LogState, Progress};
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::process::{Command, Stdio};

pub struct Outcome {
    pub success: bool,
    pub code: i32,
    pub log: LogState,
    pub stdout: String,
}

pub fn run_nix(args: &[String], raw_log: Option<&std::path::Path>) -> Result<Outcome> {
    let mut cmd = crate::util::nix();
    cmd.args(args).args(["--log-format", "internal-json", "--show-trace"]);
    let mut child = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start nix")?;

    let mut stdout = child.stdout.take().unwrap();
    let out_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });

    let tty = std::io::stderr().is_terminal();
    let mut raw = raw_log.map(std::fs::File::create).transpose()?;
    let mut log = LogState::default();
    let mut builds = 0usize;
    let mut status_shown = false;
    let stderr = BufReader::new(child.stderr.take().unwrap());
    for line in stderr.lines() {
        let line = line?;
        if let Some(f) = raw.as_mut() {
            let _ = writeln!(f, "{line}");
        }
        let status = match log.feed_line(&line) {
            Progress::Building(name) => {
                builds += 1;
                Some(format!("building {name}"))
            }
            Progress::Phase(name, phase) => Some(format!("building {name} ({phase})")),
            Progress::Message(m) => {
                if status_shown {
                    eprint!("\r\x1b[2K");
                    status_shown = false;
                }
                if tty {
                    eprintln!("{m}")
                } else {
                    eprintln!("{}", crate::util::strip_ansi(&m))
                }
                None
            }
            Progress::None => None,
        };
        if let Some(s) = status
            && tty
        {
            let mut s = format!("[{builds}] {s}");
            s.truncate(100);
            eprint!("\r\x1b[2K\x1b[2m{s}\x1b[0m");
            status_shown = true;
        }
    }
    if status_shown {
        eprint!("\r\x1b[2K");
    }
    let status = child.wait()?;
    let stdout = out_thread.join().unwrap_or_default();
    Ok(Outcome {
        success: status.success(),
        code: status.code().unwrap_or(1),
        log,
        stdout,
    })
}

pub fn sudo_prefix() -> Vec<String> {
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if uid.as_deref() == Some("0") {
        vec![]
    } else {
        vec!["sudo".into()]
    }
}
