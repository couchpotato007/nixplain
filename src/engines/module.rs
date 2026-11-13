//! Class C: module system errors. The information is in the message;
//! we only point at the right line and list every definition.

use super::Ctx;
use crate::diag::{Diagnostic, Fix, Label, Snippet};
use crate::nixast;
use crate::nixlog::NixError;
use regex::Regex;
use std::sync::LazyLock;

static UNKNOWN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^The option `([^']+)' does not exist\. Definition values:\s*(.*)$").unwrap());
static SUGGEST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Did you mean `([^']+)'").unwrap());
static CONFLICT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)^The option `([^']+)' (has conflicting definition values|is defined multiple times[^:]*):\s*(.*)$")
        .unwrap()
});
static WRONG_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)^A definition for option `([^']+)' is not of type `(.+?)'\. Definition values:\s*(.*)$").unwrap()
});
static USED_NOT_DEFINED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^The option `([^']+)' was accessed but has no value defined").unwrap());
static NOT_DEFINED_OLD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^The option `([^']+)' is used but not defined").unwrap());
static DEF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^- In `([^']+)': ?(.*)$").unwrap());

struct Def {
    file: String,
    value: String,
}

fn defs(s: &str) -> Vec<Def> {
    DEF.captures_iter(s)
        .map(|c| Def {
            file: c[1].to_string(),
            value: c[2].trim().to_string(),
        })
        .collect()
}

/// Where `option` is defined in `file` (a file Nix named), as a snippet with
/// one label. Uses the syntax tree, so nested sets and `mkIf` wrappers work.
fn locate_option(ctx: &Ctx, file: &str, option: &str) -> Option<Snippet> {
    let loc = ctx.own.locate(file);
    let path = loc.path?;
    let f = nixast::File::load(&path)?;
    let span = *nixast::find_option(&f, option, 2).first()?;
    Some(Snippet {
        title: None,
        display: loc.display,
        path,
        labels: vec![Label::primary(span.line, span.column, span.len, "")],
    })
}

fn split_path(opt: &str) -> Vec<String> {
    nixast::split_option(opt)
}

/// Every place in the configuration's own modules where one of `opts` (or a
/// path below it) is defined. One snippet per file.
fn locate_prefix(ctx: &Ctx, opts: &[String], label: &str) -> Vec<Snippet> {
    let mut out = vec![];
    for path in ctx.own.module_files() {
        let Some(f) = nixast::File::load(&path) else { continue };
        let mut labels: Vec<Label> = vec![];
        for opt in opts {
            for s in nixast::find_option(&f, opt, 2) {
                if !labels.iter().any(|l| l.line == s.line) {
                    labels.push(Label::primary(s.line, s.column, s.len, label));
                }
            }
        }
        if !labels.is_empty() {
            labels.sort_by_key(|l| (l.line, l.column));
            out.push(Snippet {
                title: None,
                display: crate::render::display_path(&path),
                path,
                labels,
            });
        }
    }
    out
}

pub fn diagnose(ctx: &Ctx, e: &NixError) -> Diagnostic {
    let m = &e.message;
    if let Some(c) = UNKNOWN.captures(m) {
        let opt = c[1].to_string();
        let suggestion = SUGGEST.captures(m).map(|s| s[1].to_string());
        let mut d = Diagnostic::new("module/unknown-option", format!("option `{opt}` does not exist"));
        d.short = format!("unknown option {opt}");
        for def in defs(&c[2]) {
            if let Some(mut s) = locate_option(ctx, &def.file, &opt) {
                s.labels[0].text = match &suggestion {
                    Some(sug) => format!("did you mean `{}`?", sug.rsplit('.').next().unwrap_or(sug)),
                    None => "no such option".into(),
                };
                d.snippet(s);
            } else {
                match locate_prefix(ctx, std::slice::from_ref(&opt), "no such option")
                    .into_iter()
                    .next()
                {
                    Some(s) => {
                        d.snippet(s);
                    }
                    None => {
                        d.note("defined in", ctx.own.locate(&def.file).display);
                    }
                }
            }
        }
        if let Some(sug) = suggestion {
            d.fixes(vec![Fix::code("rename it", format!("{sug} = …;"))]);
        } else {
            d.fixes(vec![Fix::text(
                "check the option name in the NixOS options search, or import the module that declares it",
            )]);
        }
        return d;
    }

    if let Some(c) = CONFLICT.captures(m) {
        let opt = c[1].to_string();
        let mut d = Diagnostic::new("module/conflict", format!("option `{opt}` has conflicting definitions"));
        d.short = format!("conflicting definitions for {opt}");
        let ds = defs(&c[3]);
        // Every definition gets a snippet when it is in the user's code; the
        // others (NixOS modules) are listed with their value.
        let mut unlocated = vec![];
        for def in &ds {
            let located = ctx
                .own
                .is_user(&def.file)
                .then(|| locate_option(ctx, &def.file, &opt))
                .flatten();
            match located {
                Some(mut s) => {
                    s.labels[0].text = format!("= {}", def.value);
                    d.snippet(s);
                }
                None => unlocated.push(format!("{} = {}", ctx.own.locate(&def.file).display, def.value)),
            }
        }
        d.section("also defined in", unlocated);
        let own: Vec<&Def> = ds.iter().filter(|d| ctx.own.is_user(&d.file)).collect();
        let value = own.first().map(|d| d.value.as_str()).unwrap_or("…");
        let mut fixes = vec![];
        if own.len() > 1 {
            fixes.push(Fix::text("you set it in more than one file: keep only one definition"));
        }
        fixes.push(Fix::code(
            if own.len() > 1 {
                "or let one of them win"
            } else {
                "make your definition win"
            },
            format!("{opt} = lib.mkForce {value};"),
        ));
        d.fixes(fixes);
        return d;
    }

    if let Some(c) = WRONG_TYPE.captures(m) {
        // list elements are reported as `opt."[definition 1-entry 2]"`
        static ENTRY: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"\."\[definition (\d+)-entry (\d+)\]"$"#).unwrap());
        let raw = c[1].to_string();
        let entry = ENTRY.captures(&raw).map(|e| e[2].to_string());
        let opt = ENTRY.replace(&raw, "").to_string();
        let ty = c[2].to_string();
        let what = entry.as_ref().map(|n| format!("entry {n} of ")).unwrap_or_default();
        let mut d = Diagnostic::new("module/type", format!("wrong type in {what}option `{opt}`"));
        d.short = format!("{opt}: expected {ty}");
        d.note("expected", ty.clone());
        for def in defs(&c[3]) {
            match locate_option(ctx, &def.file, &opt) {
                Some(mut s) => {
                    // a list entry: underline that element; otherwise the value
                    let file = nixast::File::load(&s.path);
                    let item = file.as_ref().and_then(|f| {
                        let n: usize = entry.as_ref()?.parse().ok()?;
                        nixast::list_items(f, &opt, 2).into_iter().nth(n - 1).map(|i| i.1)
                    });
                    match item {
                        Some(sp) => s.labels[0] = Label::primary(sp.line, sp.column, sp.len, "wrong type"),
                        None => s.labels[0].text = format!("is {}, expected {ty}", def.value),
                    }
                    d.snippet(s);
                }
                None => {
                    d.note(
                        "defined in",
                        format!("{} = {}", ctx.own.locate(&def.file).display, def.value),
                    );
                }
            }
        }
        return d;
    }

    if let Some(c) = USED_NOT_DEFINED.captures(m).or_else(|| NOT_DEFINED_OLD.captures(m)) {
        let opt = c[1].to_string();
        let mut d = Diagnostic::new("module/undefined", format!("option `{opt}` is used but has no value"));
        d.short = format!("{opt} has no value");
        let user_frame = e
            .trace
            .iter()
            .filter_map(|f| f.pos.as_ref())
            .find(|p| ctx.own.is_user(&p.file));
        if let Some(p) = user_frame
            && let Some(s) = super::snippet_at(ctx, &p.file, p.line, p.column, 1, "used here")
        {
            d.snippet(s);
        } else {
            // Usually a submodule entry (fileSystems."/data") is missing a
            // field: point at where the entry is defined.
            let parts = split_path(&opt);
            if parts.len() > 1 {
                let parent = nixast::join_option(&parts[..parts.len() - 1]);
                let last = parts.last().cloned().unwrap_or_default();
                for s in locate_prefix(ctx, &[parent], &format!("`{last}` is missing here"))
                    .into_iter()
                    .take(2)
                {
                    d.snippet(s);
                }
            }
        }
        d.fixes(vec![Fix::code("set it in your configuration", format!("{opt} = …;"))]);
        return d;
    }

    if let Some(rest) = m.strip_prefix("Failed assertions:") {
        let items: Vec<String> = rest
            .lines()
            .filter_map(|l| l.trim().strip_prefix("- "))
            .map(|l| l.trim().to_string())
            .collect();
        let title = match items.as_slice() {
            [one] => format!("NixOS assertion failed: {}", one.trim_end_matches('.')),
            _ => format!("{} NixOS assertions failed", items.len()),
        };
        let mut d = Diagnostic::new("module/assertion", title);
        d.short = items.first().cloned().unwrap_or_else(|| "assertion failed".into());
        if items.len() > 1 {
            d.section("assertions", items.iter().map(|i| format!("- {i}")).collect());
        }
        // Assertions carry no position. Point at user lines that set an
        // option the message names (e.g. `users.users.bob.isSystemUser`).
        let opts = mentioned_options(&items.join(" "));
        for s in locate_prefix(ctx, &opts, "set here").into_iter().take(3) {
            d.snippet(s);
        }
        return d;
    }

    super::eval::diagnose(ctx, e)
}

/// Option-like words in an assertion message: `a.b.c` paths, plus
/// `users.users.<name>` for "The user <name> …" messages.
fn mentioned_options(msg: &str) -> Vec<String> {
    static PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`?\b([a-z][\w-]*(?:\.[\w\x22<>-]+){1,})`?").unwrap());
    static USER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[Uu]ser '?([a-z_][\w-]*)'?").unwrap());
    let mut out: Vec<String> = PATH
        .captures_iter(msg)
        .map(|c| c[1].trim_end_matches('.').to_string())
        .filter(|p| p.contains('.') && !p.ends_with(".nix"))
        .collect();
    for c in USER.captures_iter(msg) {
        out.push(format!("users.users.{}", &c[1]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Ownership, Target};
    use crate::nixlog::{LogState, parse_str};

    #[test]
    fn unknown_option() {
        let dir = std::env::temp_dir().join(format!("nixplain-mod-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mod.nix");
        std::fs::write(
            &file,
            "{ ... }: {\n  services.pipewire = {\n    enable = true;\n    pulse.enabled = true;\n  };\n}\n",
        )
        .unwrap();
        let st = parse_str(
            &include_str!("../../tests/fixtures/module.jsonl")
                .replace("/home/user/config/mod.nix", file.to_str().unwrap()),
        );
        let own = Ownership {
            local_root: Some(dir.clone()),
            ..Default::default()
        };
        let log = LogState::default();
        let ctx = Ctx {
            target: &Target::Unknown,
            own: &own,
            log: &log,
            no_rerun: true,
        };
        let d = diagnose(&ctx, &st.errors[0]);
        assert_eq!(d.code, "module/unknown-option");
        let loc = d.location.unwrap();
        assert_eq!((loc.line, loc.column), (4, 11));
    }
}
