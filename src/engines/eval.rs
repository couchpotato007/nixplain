//! Class B: evaluation errors (type errors, missing attributes, infinite
//! recursion, ...). Stage 1 of the design: the blame boundary. We point at
//! the innermost trace frame in the user's own code instead of the frame
//! deep inside nixpkgs lib, and add hints for well-known mistakes.

use super::{Ctx, first_line};
use crate::context::Owner;
use crate::diag::{Diagnostic, Fix, Label, Snippet};
use crate::nixast;
use crate::nixlog::{NixError, Pos};
use regex::Regex;
use std::sync::LazyLock;

static EXPECTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^expected an? (\w+) but found an? (\w+)(?:: (.*))?").unwrap());
static ATTR_MISSING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^attribute '([^']+)' missing").unwrap());
static UNDEFINED_VAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^undefined variable '([^']+)'").unwrap());
static MISSING_ARG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^function '([^']+)' called without required argument '([^']+)'").unwrap());
/// `callPackage` reports missing arguments through `throw`, so the message is
/// wrapped in "evaluation aborted with the following error message: '…'".
static CALLPACKAGE_ARG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"callPackageWith: Function called without required argument "([^"]+)" at ([^,']+)(?:, did you mean (.+?))?'?$"#)
        .unwrap()
});
static UNEXPECTED_ARG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^function '([^']+)' called with unexpected argument '([^']+)'").unwrap());
static CALLING_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"while calling (?:the '([^']+)' builtin|'([^']+)')").unwrap());

struct Blame {
    pos: Pos,
    /// What Nix was doing there (the frame message).
    doing: String,
    /// The innermost non-user frame, if it differs (where it actually blew up).
    raised_in: Option<(String, String)>,
}

/// Innermost frame that belongs to the user, plus where the error was raised.
fn blame(ctx: &Ctx, e: &NixError) -> Option<Blame> {
    // The error's own position (parse errors, undefined variables) counts
    // as the innermost frame.
    let own = e.pos.clone().map(|p| (p, String::new()));
    let frames = own
        .into_iter()
        .chain(e.trace.iter().filter_map(|f| f.pos.clone().map(|p| (p, f.msg.clone()))));
    let mut raised_in = None;
    for (pos, msg) in frames {
        let loc = ctx.own.locate(&pos.file);
        match loc.owner {
            Owner::User => {
                return Some(Blame {
                    pos,
                    doing: msg,
                    raised_in,
                });
            }
            Owner::Input(_) | Owner::Unknown if raised_in.is_none() => {
                raised_in = Some((format!("{}:{}", loc.display, pos.line), msg));
            }
            _ => {}
        }
    }
    None
}

/// The function being called at the blame frame, e.g. `mapAttrs`.
fn called_function(e: &NixError) -> Option<String> {
    e.trace.iter().find_map(|f| {
        CALLING_FN
            .captures(&f.msg)
            .and_then(|c| c.get(1).or(c.get(2)))
            .map(|m| m.as_str().to_string())
    })
}

pub fn diagnose(ctx: &Ctx, e: &NixError) -> Diagnostic {
    if let Some(c) = CALLPACKAGE_ARG.captures(first_line(&e.message)) {
        return callpackage_argument(ctx, e, &c);
    }
    let msg = first_line(&e.message).to_string();
    let b = blame(ctx, e);
    let (code, title, label, hint) = analyse(e, &msg, b.as_ref(), ctx);
    let mut d = Diagnostic::new(code, title);

    if let Some(b) = &b {
        let label = label.unwrap_or_else(|| b.doing.clone());
        let special = match code {
            "eval/missing-argument" | "eval/unexpected-argument" => argument_snippets(ctx, e, b, &msg),
            "eval/syntax" => missing_semicolon(ctx, b, &msg).map(|(title, s)| {
                d.title = title;
                vec![s]
            }),
            _ => None,
        };
        match special {
            Some(snippets) => {
                for s in snippets {
                    d.snippet(s);
                }
            }
            None => {
                let loc = ctx.own.locate(&b.pos.file);
                if let Some(path) = loc.path.clone() {
                    let src = std::fs::read_to_string(&path).unwrap_or_default();
                    let text = src.lines().nth(b.pos.line.saturating_sub(1)).unwrap_or("");
                    let len = token_len(text, b.pos.column);
                    d.snippet(Snippet {
                        title: None,
                        display: loc.display.clone(),
                        path,
                        labels: vec![Label::primary(b.pos.line, b.pos.column, len, label)],
                    });
                }
            }
        }
        if let Some((where_, what)) = &b.raised_in
            && !code.ends_with("infinite-recursion")
        {
            let what = if what.is_empty() {
                String::new()
            } else {
                format!(" ({what})")
            };
            d.note("raised in", format!("{where_}{what}"));
        }
    } else if let Some(f) = e.trace.iter().find_map(|f| f.pos.clone()) {
        d.note(
            "at",
            format!("{}:{} (no frame in your code)", ctx.own.locate(&f.file).display, f.line),
        );
    }
    let rest: Vec<&str> = e
        .message
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .take(6)
        .collect();
    if !rest.is_empty() {
        d.section("details", rest.iter().map(|s| s.to_string()).collect());
    }
    if let Some(h) = hint {
        d.fixes(vec![h]);
    }
    d
}

fn callpackage_argument(ctx: &Ctx, e: &NixError, c: &regex::Captures) -> Diagnostic {
    let arg = c[1].to_string();
    let suggestions: Vec<String> = c
        .get(3)
        .map(|m| {
            m.as_str()
                .split([',', '?'])
                .flat_map(|s| s.split(" or "))
                .map(|s| s.trim().trim_matches('"').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut d = Diagnostic::new(
        "eval/callpackage-argument",
        format!("callPackage cannot find `{arg}` in pkgs"),
    );
    d.short = format!("callPackage: no package `{arg}`");

    // `at /nix/store/…-source/pkgs/x.nix:2:1` → the function's file
    let file = c[2].trim().split(':').next().unwrap_or("").to_string();
    let loc = ctx.own.locate(&file);
    match loc.path.clone().filter(|_| loc.owner == Owner::User) {
        Some(path) => match nixast::File::load(&path).and_then(|f| nixast::pattern_entry(&f, &arg)) {
            Some(sp) => {
                d.snippet(Snippet {
                    title: None,
                    display: loc.display.clone(),
                    path,
                    labels: vec![Label::primary(sp.line, sp.column, sp.len, "no package with this name")],
                });
            }
            None => {
                d.note("in", loc.display.clone());
            }
        },
        None => {
            if let Some(b) = blame(ctx, e) {
                d.note(
                    "called from",
                    format!("{}:{}", ctx.own.locate(&b.pos.file).display, b.pos.line),
                );
            }
        }
    }
    // where callPackage is called (first frame in the user's code)
    if let Some(p) = e
        .trace
        .iter()
        .filter_map(|f| f.pos.as_ref())
        .find(|p| ctx.own.is_user(&p.file))
        && let Some(path) = ctx.own.locate(&p.file).path
    {
        let len = nixast::File::load(&path)
            .and_then(|f| nixast::call_span(&f, f.offset(p.line, p.column)?))
            .map(|sp| sp.len)
            .unwrap_or(1);
        d.snippet(Snippet {
            title: Some("called here".into()),
            display: ctx.own.locate(&p.file).display,
            path,
            labels: vec![Label::secondary(p.line, p.column, len.max(1), "")],
        });
    }
    let mut fixes = vec![];
    if !suggestions.is_empty() {
        let quoted: Vec<String> = suggestions.iter().map(|s| format!("`{s}`")).collect();
        fixes.push(Fix::text(format!("did you mean {}?", quoted.join(", "))));
    }
    fixes.push(Fix::code(
        "pass it explicitly",
        format!("callPackage <file> {{ {arg} = …; }}"),
    ));
    fixes.push(Fix::text(format!("or add `{arg}` to pkgs with an overlay")));
    d.fixes(fixes);
    d
}

static ARG_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"argument '([^']+)'").unwrap());

/// Argument errors: Nix reports the function definition, but the user has to
/// change the call. Primary: the call (or the passed attribute for an
/// unexpected argument). Secondary: the definition.
fn argument_snippets(ctx: &Ctx, e: &NixError, b: &Blame, msg: &str) -> Option<Vec<Snippet>> {
    let arg = ARG_NAME.captures(msg)?[1].to_string();
    let missing = msg.contains("without required argument");
    let call = e
        .trace
        .iter()
        .filter(|f| f.msg.contains("call site"))
        .filter_map(|f| f.pos.as_ref())
        .find(|p| ctx.own.is_user(&p.file) && (p.file != b.pos.file || p.line != b.pos.line))?;
    let call_loc = ctx.own.locate(&call.file);
    let call_path = call_loc.path.clone()?;
    let call_file = nixast::File::load(&call_path)?;
    let call_off = call_file.offset(call.line, call.column)?;
    // the called function as written: `mkUser`, `lib.foo`
    let func = call_file
        .root
        .descendants()
        .filter(|n| n.kind() == rnix::SyntaxKind::NODE_APPLY && usize::from(n.text_range().start()) == call_off)
        .max_by_key(|n| n.text_range().len())
        .and_then(|n| nixast::apply_parts(&n))
        .map(|(f, _)| f)
        .unwrap_or_else(|| "the function".into());
    let call_sp = call_file.span(rnix::TextRange::at(
        rnix::TextSize::from(call_off as u32),
        rnix::TextSize::from(func.len() as u32),
    ));

    // primary: for an unexpected argument the `arg = …` inside the call
    let passed = if missing {
        None
    } else {
        nixast::call_argument(&call_file, call_off, &arg)
    };
    let primary = match passed {
        Some(sp) => Label::primary(sp.line, sp.column, sp.len, format!("`{func}` has no argument `{arg}`")),
        None => Label::primary(
            call_sp.line,
            call_sp.column,
            call_sp.len,
            format!("`{arg}` is not passed here"),
        ),
    };

    // secondary: the argument pattern of the definition
    let def_loc = ctx.own.locate(&b.pos.file);
    let def_path = def_loc.path.clone()?;
    let def_file = nixast::File::load(&def_path)?;
    let def_off = def_file.offset(b.pos.line, b.pos.column)?;
    let (def_sp, set) = nixast::lambda_pattern(&def_file, def_off)?;
    let def_label = Label::secondary(def_sp.line, def_sp.column, def_sp.len, format!("`{func}` takes {set}"));

    if def_path == call_path {
        return Some(vec![Snippet {
            title: None,
            display: call_loc.display,
            path: call_path,
            labels: vec![primary, def_label],
        }]);
    }
    Some(vec![
        Snippet {
            title: None,
            display: call_loc.display,
            path: call_path,
            labels: vec![primary],
        },
        Snippet {
            title: Some(format!("`{func}` is defined here")),
            display: def_loc.display,
            path: def_path,
            labels: vec![def_label],
        },
    ])
}

/// "unexpected X, expecting ';'": Nix stops at the next token, but the `;` is
/// missing at the end of the previous line. Primary: where the `;` goes.
fn missing_semicolon(ctx: &Ctx, b: &Blame, msg: &str) -> Option<(String, Snippet)> {
    if !msg.contains("expecting ';'") {
        return None;
    }
    let loc = ctx.own.locate(&b.pos.file);
    let path = loc.path.clone()?;
    let src = std::fs::read_to_string(&path).ok()?;
    let lines: Vec<&str> = src.lines().collect();
    let (i, prev) = lines[..b.pos.line.saturating_sub(1).min(lines.len())]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| !l.trim().is_empty())?;
    let col = prev.trim_end().chars().count() + 1;
    let here = lines.get(b.pos.line - 1).copied().unwrap_or("");
    let snippet = Snippet {
        title: None,
        display: loc.display,
        path,
        labels: vec![
            Label::primary(i + 1, col, 1, "add `;` here"),
            Label::secondary(
                b.pos.line,
                b.pos.column,
                token_len(here, b.pos.column),
                "Nix noticed it only here",
            ),
        ],
    };
    Some((format!("missing `;` at the end of line {}", i + 1), snippet))
}

/// Closest package names in the system's pkgs (edit distance ≤ 2).
fn suggest_package(ctx: &Ctx, name: &str) -> Option<String> {
    if ctx.no_rerun {
        return None;
    }
    let crate::context::Target::Nixos { flake, host } = ctx.target else {
        return None;
    };
    let attr = format!("{flake}#nixosConfigurations.\"{host}\".pkgs");
    let out =
        crate::util::run(crate::util::nix().args(["eval", "--json", &attr, "--apply", "builtins.attrNames"])).ok()?;
    let names: Vec<String> = serde_json::from_str(&out).ok()?;
    // Among equally close names prefer one that contains every typed letter
    // in order (`helo` → `hello`, not `halo`).
    let subseq = |short: &str, long: &str| {
        let mut it = long.chars();
        short.chars().all(|c| it.any(|x| x == c))
    };
    names
        .into_iter()
        .map(|n| {
            let typo = !(subseq(name, &n) || subseq(&n, name));
            (crate::util::levenshtein(&n, name), typo, n)
        })
        .filter(|(dist, _, _)| *dist <= 2)
        .min()
        .map(|(_, _, n)| n)
}

/// Length of the expression token starting at `col` (for the underline).
fn token_len(line: &str, col: usize) -> usize {
    let rest: String = line.chars().skip(col.saturating_sub(1)).collect();
    let n = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || "_.-'".contains(*c))
        .count();
    n.max(1)
}

/// The innermost binding whose value contains the blame position, as
/// (option path, value). E.g. `networking.domain = config.networking.domain;`.
fn blame_binding(ctx: &Ctx, b: Option<&Blame>) -> Option<(Vec<String>, rnix::SyntaxNode)> {
    let b = b?;
    let f = nixast::File::load(&ctx.own.locate(&b.pos.file).path?)?;
    let off = rnix::TextSize::from(f.offset(b.pos.line, b.pos.column)? as u32);
    f.defs()
        .into_iter()
        .filter_map(|d| {
            let v = d.value.clone()?;
            let r = v.text_range();
            (r.start() <= off && off <= r.end()).then(|| (d.option_path().iter().map(|s| s.to_string()).collect(), v))
        })
        .min_by_key(|(_, v): &(Vec<String>, rnix::SyntaxNode)| v.text_range().len())
}

fn analyse(
    e: &NixError,
    msg: &str,
    b: Option<&Blame>,
    ctx: &Ctx,
) -> (&'static str, String, Option<String>, Option<Fix>) {
    let binding = blame_binding(ctx, b);
    if msg.starts_with("infinite recursion encountered") {
        if let Some((path, value)) = &binding {
            let opt = nixast::join_option(path);
            if value.text() == format!("config.{opt}").as_str() {
                return (
                    "eval/infinite-recursion",
                    format!("`{opt}` is defined through itself"),
                    Some("reads the option it defines".into()),
                    Some(Fix::text(format!(
                        "set `{opt}` to a plain value, or compute it from a different option"
                    ))),
                );
            }
            let text = value.text().to_string();
            if path.first().map(String::as_str) == Some("imports")
                && (text.contains("config.") || text.contains("pkgs"))
            {
                // `lib.optional config.my.gui ./gui.nix` → use the real names
                let (cond, file) = nixast::apply_parts(value)
                    .filter(|(f, args)| (f.ends_with("optional") || f.ends_with("optionals")) && args.len() == 2)
                    .and_then(|(_, args)| {
                        let file = args.get(1)?.text().to_string();
                        Some((
                            args[0].text().to_string(),
                            file.trim_matches(|c| c == '[' || c == ']' || c == ' ').to_string(),
                        ))
                    })
                    .unwrap_or(("config.my.option".into(), "./module.nix".into()));
                return (
                    "eval/infinite-recursion",
                    "cycle through `config` in `imports`".into(),
                    Some("imports must not depend on config or pkgs".into()),
                    Some(Fix::code(
                        format!("always import {file} and make its content conditional inside it"),
                        format!("imports = [ {file} ];\n\n# in {file}:\nconfig = lib.mkIf {cond} {{ … }};"),
                    )),
                );
            }
        }
        let cycle = repeating_frames(e).filter(|c| c != "the attribute 'value'");
        let hint = Fix::text(format!(
            "a value depends on itself{}; common causes: `config` used in `imports`, a module reading the option it defines without `mkIf`, an overlay using `final` where `prev` is needed",
            cycle.map(|c| format!(" (cycle: {c})")).unwrap_or_default()
        ));
        return (
            "eval/infinite-recursion",
            "infinite recursion".into(),
            Some("the cycle passes through here".into()),
            Some(hint),
        );
    }
    if let Some(c) = EXPECTED.captures(msg) {
        let (want, got) = (c[1].to_string(), c[2].to_string());
        let func = called_function(e);
        // "while evaluating the second argument passed to builtins.mapAttrs"
        static NTH: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"while evaluating the (\w+) argument passed to builtins\.(\w+)").unwrap());
        let nth = e
            .trace
            .iter()
            .find_map(|f| NTH.captures(&f.msg).map(|c| c[1].to_string()));
        let title = match (&func, &nth) {
            (Some(f), Some(n)) => format!("`{f}` expects a {want} as its {n} argument, but got a {got}"),
            (Some(f), None) => format!("`{f}` expects a {want}, but got a {got}"),
            _ => format!("expected a {want}, but got a {got}"),
        };
        let value = c
            .get(3)
            .map(|v| v.as_str())
            .filter(|v| !v.contains('«'))
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        let label = match (&func, &nth) {
            (Some(_), Some(n)) => format!("the {n} argument is a {got}{value}"),
            (Some(_), None) => format!("called with a {got}{value}"),
            _ => format!("this is a {got}{value}"),
        };
        let hint = match (want.as_str(), got.as_str(), func.as_deref()) {
            ("set", "list", Some("mapAttrs")) => Some(Fix::code(
                "to build an attribute set from a list of names",
                "lib.genAttrs names (name: …)",
            )),
            ("set", "list", _) => Some(Fix::code(
                "to turn a list into an attribute set",
                "lib.listToAttrs (map (x: lib.nameValuePair x.name x) list)",
            )),
            ("list", "set", _) => Some(Fix::code(
                "to turn an attribute set into a list",
                "lib.attrValues set   # or lib.mapAttrsToList (name: value: …) set",
            )),
            ("string", "set", _) => Some(Fix::text(
                "a package or attribute set was used where a string is expected; use \"${pkg}\" or pkg.outPath, or pick an attribute",
            )),
            ("set", "lambda", _) | ("list", "lambda", _) => Some(Fix::text(
                "a function was used as a value; did you forget to call it, e.g. `import ./x.nix { … }`?",
            )),
            _ => None,
        };
        return ("eval/type", title, Some(label), hint);
    }
    if let Some(c) = ATTR_MISSING.captures(msg) {
        let a = c[1].to_string();
        let in_pkgs = binding
            .as_ref()
            .is_some_and(|(_, v)| v.text().to_string().contains(&format!("pkgs.{a}")));
        let hint = match in_pkgs.then(|| suggest_package(ctx, &a)).flatten() {
            Some(s) => Fix::code("did you mean", format!("pkgs.{s}")),
            None if in_pkgs => Fix::text(format!(
                "no package `{a}` in this nixpkgs; search https://search.nixos.org/packages?query={a}"
            )),
            None => Fix::code(
                "check the name; if the attribute may be absent, use a default",
                format!("x.{a} or null"),
            ),
        };
        let what = if in_pkgs { "package" } else { "attribute" };
        return (
            "eval/missing-attribute",
            format!("{what} `{a}` does not exist"),
            Some(format!("no {what} `{a}`")),
            Some(hint),
        );
    }
    if let Some(c) = UNDEFINED_VAR.captures(msg) {
        let v = c[1].to_string();
        if binding.as_ref().is_some_and(|(_, v)| {
            v.kind() == rnix::SyntaxKind::NODE_WITH && v.text().to_string().starts_with("with pkgs")
        }) && let Some(s) = suggest_package(ctx, &v)
        {
            return (
                "eval/undefined-variable",
                format!("undefined variable `{v}`"),
                Some("not a package and not in scope".into()),
                Some(Fix::code("did you mean the package", s)),
            );
        }
        return (
            "eval/undefined-variable",
            format!("undefined variable `{v}`"),
            Some("not in scope".into()),
            Some(Fix::text(format!(
                "add `{v}` to the function arguments (e.g. `{{ pkgs, lib, {v}, ... }}:`), or qualify it (`pkgs.{v}`, `lib.{v}`)"
            ))),
        );
    }
    if let Some(c) = MISSING_ARG.captures(msg) {
        let (f, a) = (c[1].to_string(), c[2].to_string());
        return (
            "eval/missing-argument",
            format!("`{f}` is called without its argument `{a}`"),
            Some(format!("`{a}` is missing")),
            Some(Fix::text(format!(
                "pass `{a} = …;` in the call, or give it a default in the function: `{a} ? …`"
            ))),
        );
    }
    if let Some(c) = UNEXPECTED_ARG.captures(msg) {
        let (f, a) = (c[1].to_string(), c[2].to_string());
        return (
            "eval/unexpected-argument",
            format!("`{f}` is called with `{a}`, which it does not take"),
            Some(format!("`{a}` passed here")),
            Some(Fix::text(format!(
                "remove `{a}` from the call, or accept it in the function: `{a} ? …` or `...`"
            ))),
        );
    }
    if msg.starts_with("syntax error") {
        return ("eval/syntax", msg.to_string(), Some("unexpected here".into()), None);
    }
    if matches!(called_function(e).as_deref(), Some("throw") | Some("abort")) {
        let m = msg
            .strip_prefix("evaluation aborted with the following error message: '")
            .map(|m| m.trim_end_matches('\''))
            .unwrap_or(msg);
        return ("eval/throw", m.to_string(), Some("thrown here".into()), None);
    }
    ("eval/error", msg.to_string(), None, None)
}

/// Frame messages that repeat in the trace, e.g. an option evaluated twice.
fn repeating_frames(e: &NixError) -> Option<String> {
    let mut seen = std::collections::HashMap::new();
    for f in &e.trace {
        if f.msg.contains("while evaluating the option") || f.msg.contains("while evaluating the attribute") {
            *seen.entry(f.msg.clone()).or_insert(0) += 1;
        }
    }
    seen.into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(m, _)| m.trim_start_matches("while evaluating ").to_string())
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Ownership, Target};
    use crate::nixlog::{LogState, parse_str};

    #[test]
    fn type_error_hint() {
        let st = parse_str(include_str!("../../tests/fixtures/type.jsonl"));
        let own = Ownership::default();
        let log = LogState::default();
        let ctx = Ctx {
            target: &Target::Unknown,
            own: &own,
            log: &log,
            no_rerun: true,
        };
        let d = diagnose(&ctx, &st.errors[0]);
        assert_eq!(d.code, "eval/type");
        assert_eq!(
            d.title,
            "`mapAttrs` expects a set as its second argument, but got a list"
        );
        assert!(
            matches!(d.blocks.last(), Some(crate::diag::Block::Fixes { fixes: f }) if f[0].code.as_deref() == Some("lib.genAttrs names (name: …)"))
        );
    }
}
