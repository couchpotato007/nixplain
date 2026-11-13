//! Small helpers shared by all modules: ANSI stripping, store-path names,
//! running external commands.

use anyhow::{Context, Result, bail};
use regex::Regex;
use std::process::{Command, Stdio};
use std::sync::LazyLock;

/// CSI sequences (colours) and OSC sequences (e.g. GCC's `\e]8;;url\e\\` links).
static ANSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").unwrap());

pub fn strip_ansi(s: &str) -> String {
    ANSI.replace_all(s, "").into_owned()
}

/// `/nix/store/<hash>-foo-1.2.drv` or `<hash>-foo-1.2.drv` -> `foo-1.2`
pub fn drv_name(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let base = base.strip_suffix(".drv").unwrap_or(base);
    match base.split_once('-') {
        Some((hash, rest)) if hash.len() == 32 => rest.to_string(),
        _ => base.to_string(),
    }
}

/// Split `foo-bar-1.2.3` into (`foo-bar`, Some(`1.2.3`)), following Nix's
/// `builtins.parseDrvName` rule: the version starts at the first `-`
/// that is followed by a digit.
pub fn split_name_version(name: &str) -> (String, Option<String>) {
    let bytes = name.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'-' && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            return (name[..i].to_string(), Some(name[i + 1..].to_string()));
        }
    }
    (name.to_string(), None)
}

pub fn pname(name: &str) -> String {
    split_name_version(name).0
}

/// Run a command, return stdout. Stderr is captured and included in the error.
pub fn run(cmd: &mut Command) -> Result<String> {
    let desc = describe(cmd);
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to spawn {desc}"))?;
    if !out.status.success() {
        let err = strip_ansi(&String::from_utf8_lossy(&out.stderr));
        let tail: Vec<&str> = err.lines().rev().take(6).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!("{desc} failed:\n{}", tail.join("\n"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `nix eval --json --impure --expr <…>`: the command without environment
/// and with long arguments (generated Nix expressions) elided.
fn describe(cmd: &Command) -> String {
    let mut parts = vec![cmd.get_program().to_string_lossy().into_owned()];
    let mut args = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).peekable();
    while let Some(a) = args.next() {
        if a == "--extra-experimental-features" {
            args.next();
            continue;
        }
        parts.push(if a.len() > 80 || a.contains('\n') { "<…>".into() } else { a });
    }
    format!("`{}`", parts.join(" "))
}

/// A `nix` command with the experimental features nixplain relies on.
pub fn nix() -> Command {
    let mut c = Command::new("nix");
    c.args(["--extra-experimental-features", "nix-command flakes"]);
    c
}

/// Quote a string as a Nix string literal.
pub fn nix_str(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Edit distance between two short strings.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            drv_name("/nix/store/1kzyzxzsgcmm8vsr6w3dbm3fdpbmx30x-failer.drv"),
            "failer"
        );
        assert_eq!(
            drv_name("0mvzg2dqivqc8ndvy2dxsh1zx9385ysn-hello-2.12.3.drv"),
            "hello-2.12.3"
        );
        assert_eq!(split_name_version("python3.12-somelib-2.1").0, "python3.12-somelib");
        assert_eq!(split_name_version("olm-3.2.16").1.as_deref(), Some("3.2.16"));
        assert_eq!(pname("system-path"), "system-path");
        assert_eq!(levenshtein("helo", "hello"), 1);
        assert_eq!(
            strip_ansi("x [\x1b]8;;https://gcc.gnu.org/x\x07-Wfoo\x1b]8;;\x07] y"),
            "x [-Wfoo] y"
        );
        assert_eq!(levenshtein("helloo", "hello"), 1);
    }
}
