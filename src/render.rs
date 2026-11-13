//! Renderers: rustc/Elm-style human output, one-line short output, JSON.

use crate::diag::{Block, Diagnostic, Fix, Snippet};
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static COLOR: AtomicBool = AtomicBool::new(false);

pub fn set_color(on: bool) {
    COLOR.store(on, Ordering::Relaxed);
}

fn paint(code: &str, s: &str) -> String {
    if COLOR.load(Ordering::Relaxed) {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
fn red(s: &str) -> String {
    paint("1;31", s)
}
fn bold(s: &str) -> String {
    paint("1", s)
}
fn blue(s: &str) -> String {
    paint("1;34", s)
}
fn green(s: &str) -> String {
    paint("32", s)
}
fn dim(s: &str) -> String {
    paint("2", s)
}

pub fn human(d: &Diagnostic) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}{} {}",
        red(&format!("error[{}]", d.code)),
        bold(":"),
        bold(&d.title)
    );
    let mut prev_note = true;
    for b in &d.blocks {
        match b {
            Block::Note { key, value } => {
                if !prev_note {
                    out.push('\n');
                }
                let mut lines = value.lines();
                let _ = writeln!(out, "  {} {}", bold(&format!("{key}:")), lines.next().unwrap_or(""));
                let indent = " ".repeat(key.chars().count() + 4);
                for l in lines {
                    let _ = writeln!(out, "{indent}{l}");
                }
                prev_note = true;
                continue;
            }
            Block::Section { title, lines } => {
                out.push('\n');
                let _ = writeln!(out, "  {}", bold(&format!("{title}:")));
                for l in lines {
                    let _ = writeln!(out, "    {l}");
                }
            }
            Block::Snippet(s) => {
                out.push('\n');
                render_snippet(&mut out, s);
            }
            Block::Fixes { fixes } => {
                out.push('\n');
                render_fixes(&mut out, fixes);
            }
        }
        prev_note = false;
    }
    out
}

fn render_fixes(out: &mut String, fixes: &[Fix]) {
    let _ = writeln!(out, "  {}", bold(if fixes.len() == 1 { "fix:" } else { "fixes:" }));
    for (i, f) in fixes.iter().enumerate() {
        let prefix = if fixes.len() == 1 {
            String::new()
        } else {
            format!("{}. ", i + 1)
        };
        let colon = if f.code.is_some() { ":" } else { "" };
        let _ = writeln!(out, "    {prefix}{}{colon}", f.title);
        if let Some(code) = &f.code {
            let pad = " ".repeat(4 + prefix.len() + 2);
            for l in code.lines() {
                let _ = writeln!(out, "{pad}{}", green(l));
            }
        }
    }
}

fn render_snippet(out: &mut String, s: &Snippet) {
    if let Some(t) = &s.title {
        let _ = writeln!(out, "  {}", bold(&format!("{t}:")));
    }
    let source = std::fs::read_to_string(&s.path).ok();
    let src_lines: Vec<&str> = source.as_deref().map(|s| s.lines().collect()).unwrap_or_default();

    let mut labels = s.labels.clone();
    labels.sort_by_key(|l| (l.line, l.column));
    let width = labels.iter().map(|l| l.line.to_string().len()).max().unwrap_or(1);
    let pad = " ".repeat(width + 2);
    let gutter = blue("│");

    let head = labels.iter().find(|l| l.primary).or(labels.first());
    let loc = match head {
        Some(l) => format!("{}:{}:{}", s.display, l.line, l.column),
        None => s.display.clone(),
    };
    let _ = writeln!(out, "{pad}{} {loc}", blue("┌─"));
    if src_lines.is_empty() {
        for l in &labels {
            let _ = writeln!(out, "{pad}{gutter} line {}: {}", l.line, l.text);
        }
        return;
    }
    let _ = writeln!(out, "{pad}{gutter}");

    let mut last_line: Option<usize> = None;
    let mut i = 0;
    while i < labels.len() {
        let line_no = labels[i].line;
        if let Some(prev) = last_line
            && line_no > prev + 1
        {
            let _ = writeln!(out, "{pad}{}", blue("⋮"));
        }
        let text = src_lines.get(line_no.saturating_sub(1)).copied().unwrap_or("");
        let text = text.replace('\t', " ");
        let num = format!("{:>width$}", line_no);
        let _ = writeln!(out, " {} {gutter} {text}", blue(&num));
        while i < labels.len() && labels[i].line == line_no {
            let l = &labels[i];
            let spaces = " ".repeat(l.column.saturating_sub(1));
            let mark = if l.primary { "^" } else { "-" }.repeat(l.len);
            let mark = if l.primary { red(&mark) } else { blue(&mark) };
            let mut label_lines = l.text.lines();
            let first = label_lines.next().unwrap_or("");
            let lt = if l.primary { red(first) } else { blue(first) };
            let _ = writeln!(out, "{pad}{gutter} {spaces}{mark} {lt}");
            let cont = " ".repeat(l.column.saturating_sub(1) + l.len + 1);
            for extra in label_lines {
                let _ = writeln!(
                    out,
                    "{pad}{gutter} {cont}{}",
                    if l.primary { red(extra) } else { blue(extra) }
                );
            }
            i += 1;
        }
        last_line = Some(line_no);
    }
}

/// Nix's formatted message, without colours when they are off.
pub fn nix_message(formatted: &str) -> String {
    let s = if COLOR.load(Ordering::Relaxed) {
        formatted.to_string()
    } else {
        crate::util::strip_ansi(formatted)
    };
    s.trim_end().to_string()
}

/// `──── title ────────…`
pub fn separator(title: &str) -> String {
    dim(&format!("──── {title} {}", "─".repeat(60)))
}

static STORE_PATH: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"/nix/store/[^\s'`\x22]+").unwrap());

/// The trace one line per frame, outermost first like Nix prints it. Frames
/// in the user's own files are marked; nixpkgs internals are dimmed.
pub fn trace_short(e: &crate::nixlog::NixError, own: &crate::context::Ownership) -> String {
    use crate::context::Owner;
    let mut out = String::new();
    let mut prev = String::new();
    for f in e.trace.iter().rev() {
        let loc = f.pos.as_ref().map(|p| (own.locate(&p.file), p.line));
        // store paths inside the message (`definitions from /nix/store/…`)
        let mut msg_mine = false;
        let msg = STORE_PATH
            .replace_all(f.msg.lines().next().unwrap_or(""), |c: &regex::Captures| {
                let l = own.locate(&c[0]);
                msg_mine |= l.owner == Owner::User;
                l.display
            })
            .into_owned();
        let mine = msg_mine || loc.as_ref().is_some_and(|(l, _)| l.owner == Owner::User);
        let at = loc
            .map(|(l, line)| format!("  {}:{line}", l.display))
            .unwrap_or_default();
        let line = format!("… {msg}{at}");
        if line == prev {
            continue; // Nix repeats frames for nested calls
        }
        prev = line.clone();
        let _ = if mine {
            writeln!(out, "{} {}", blue("▶"), bold(&line))
        } else {
            writeln!(out, "  {}", dim(&line))
        };
    }
    if let Some(p) = &e.pos {
        let l = own.locate(&p.file);
        let mark = if l.owner == Owner::User {
            blue("▶")
        } else {
            " ".into()
        };
        let _ = writeln!(out, "{mark} {}", bold(&format!("at {}:{}", l.display, p.line)));
    }
    let _ = writeln!(out, "{} {}", red("error:"), e.message.lines().next().unwrap_or(""));
    out
}

pub fn short(d: &Diagnostic) -> String {
    let loc = d
        .location
        .as_ref()
        .map(|l| format!("{}:{}", display_path(&l.file), l.line))
        .filter(|l| !d.short.contains(l.as_str()))
        .map(|l| format!(" ({l})"))
        .unwrap_or_default();
    format!("{} {}{}", red("✗"), d.short, dim(&loc))
}

pub fn json(diags: &[Diagnostic]) -> String {
    serde_json::to_string_pretty(diags).expect("diagnostics serialize to JSON")
}

pub fn display_path(p: &std::path::Path) -> String {
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(rel) = p.strip_prefix(&cwd)
    {
        return rel.display().to_string();
    }
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::*;

    #[test]
    fn short_trace_marks_user_frames() {
        use crate::context::{Owner, Ownership};
        use crate::nixlog::{Frame, NixError, Pos};
        let own = Ownership {
            roots: vec![("/nix/store/aaaa-source".into(), Owner::User)],
            local_root: Some("/home/u/cfg".into()),
            ..Default::default()
        };
        let frame = |file: &str, msg: &str| Frame {
            pos: Some(Pos {
                file: file.into(),
                line: 8,
                column: 1,
            }),
            msg: msg.into(),
        };
        let e = NixError {
            message: "expected a set but found a list".into(),
            formatted: String::new(),
            pos: None,
            // innermost first, as Nix's JSON has it
            trace: vec![
                frame(
                    "/nix/store/aaaa-source/hosts/a.nix",
                    "while calling the 'mapAttrs' builtin",
                ),
                Frame {
                    pos: None,
                    msg: "while evaluating definitions from `/nix/store/aaaa-source/hosts/a.nix':".into(),
                },
                frame(
                    "/nix/store/bbbb-source/lib/modules.nix",
                    "while calling 'dischargeProperties'",
                ),
            ],
        };
        let out = trace_short(&e, &own);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "  … while calling 'dischargeProperties'  <nixpkgs>/lib/modules.nix:8"
        );
        assert!(lines[1].starts_with("▶ … while evaluating definitions from `"));
        assert!(lines[1].contains("/home/u/cfg/hosts/a.nix"));
        assert_eq!(
            lines[2],
            "▶ … while calling the 'mapAttrs' builtin  /home/u/cfg/hosts/a.nix:8"
        );
        assert_eq!(lines[3], "error: expected a set but found a list");
    }

    #[test]
    fn json_roundtrip() {
        let mut d = Diagnostic::new("x/y", "t");
        d.note("k", "v").fixes(vec![Fix::text("a")]);
        let v: serde_json::Value = serde_json::from_str(&json(&[d])).unwrap();
        assert_eq!(v[0]["blocks"][1]["kind"], "fixes");
        assert_eq!(v[0]["blocks"][1]["fixes"][0]["title"], "a");
    }

    #[test]
    fn snippet_layout() {
        let dir = std::env::temp_dir().join(format!("nixplain-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.nix");
        std::fs::write(
            &f,
            "{\n  version = \"0.8.0\";\n  x = 1;\n  y = 2;\n  hash = \"sha256-AAAA\";\n}\n",
        )
        .unwrap();
        let mut d = Diagnostic::new("fetch/hash", "hash mismatch");
        d.snippet(Snippet {
            title: None,
            display: "a.nix".into(),
            path: f,
            labels: vec![
                Label::secondary(2, 13, 7, "version"),
                Label::primary(5, 10, 13, "outdated"),
            ],
        });
        let s = human(&d);
        let expected = "\
error[fetch/hash]: hash mismatch

   ┌─ a.nix:5:10
   │
 2 │   version = \"0.8.0\";
   │             ------- version
   ⋮
 5 │   hash = \"sha256-AAAA\";
   │          ^^^^^^^^^^^^^ outdated
";
        pretty_assertions::assert_eq!(s, expected);
    }
}
