//! Class A: insecure / unfree / broken / unsupported packages.
//!
//! check-meta.nix only knows the offending package. We re-evaluate with
//! the checks relaxed, walk the derivation graph from the system to the
//! package, and map each entry point back to the option and file that
//! added it.

use super::{Ctx, first_line, wrap};
use crate::context::{Owner, Target};
use crate::diag::{Diagnostic, Fix, Label, Snippet};
use crate::graph::{self, DrvGraph};
use crate::nixast;
use crate::nixlog::NixError;
use crate::util::{pname, split_name_version};
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Insecure,
    Unfree(String),
    Broken,
    Unsupported,
    Blocklisted(String),
    NonSource,
    /// Other `meta.problems` kinds.
    Problem(String),
}

#[derive(Debug, Clone)]
pub struct MetaError {
    pub package: String,
    pub kind: Kind,
    pub reason: Option<String>,
}

static HEAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^Refusing to evaluate package '([^']+)' in .+? because it (.*)$").unwrap());
static LICENSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"license \(‘(.*?)’\)").unwrap());
static PROBLEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*- (\w[\w-]*): (.+)$").unwrap());

pub fn parse(msg: &str) -> Option<MetaError> {
    let c = HEAD.captures(msg)?;
    let package = c[1].to_string();
    let rest = &c[2];
    let lic = || LICENSE.captures(rest).map(|c| c[1].to_string()).unwrap_or_default();
    let (kind, reason) = if rest.starts_with("is marked as insecure") {
        (Kind::Insecure, insecure_reason(rest))
    } else if rest.starts_with("has an unfree license") {
        let l = lic();
        (Kind::Unfree(l.clone()), Some(format!("unfree license ({l})")))
    } else if rest.starts_with("has a blocklisted license") {
        let l = lic();
        (Kind::Blocklisted(l.clone()), Some(format!("blocklisted license ({l})")))
    } else if rest.starts_with("contains elements not built from source") {
        (
            Kind::NonSource,
            Some(first_line(rest).trim_start_matches("contains ").to_string()),
        )
    } else if rest.starts_with("is not available on the requested hostPlatform") {
        (Kind::Unsupported, unsupported_reason(rest))
    } else if rest.starts_with("has problems") {
        match PROBLEM.captures(rest) {
            Some(p) if &p[1] == "broken" => (Kind::Broken, Some(p[2].trim().to_string())),
            Some(p) => (Kind::Problem(p[1].to_string()), Some(p[2].trim().to_string())),
            None => (Kind::Problem("problem".into()), None),
        }
    } else if rest.starts_with("is marked as broken") {
        (Kind::Broken, None)
    } else {
        (Kind::Problem("meta".into()), Some(first_line(rest).to_string()))
    };
    Some(MetaError { package, kind, reason })
}

/// `only for aarch64-darwin (this system: x86_64-linux)`
fn unsupported_reason(rest: &str) -> Option<String> {
    static SYS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"hostPlatform\.system = "([^"]+)""#).unwrap());
    static PLAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?s)meta\.platforms = \[(.*?)\]"#).unwrap());
    static BAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?s)meta\.badPlatforms = \[(.*?)\]"#).unwrap());
    let list = |re: &Regex| -> Vec<String> {
        re.captures(rest)
            .map(|c| {
                c[1].split_whitespace()
                    .map(|s| s.trim_matches('"').to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let sys = SYS.captures(rest).map(|c| c[1].to_string()).unwrap_or_default();
    let (ok, bad) = (list(&PLAT), list(&BAD));
    let mut v = vec![];
    if !ok.is_empty() {
        let shown: Vec<String> = ok.iter().take(6).cloned().collect();
        let more = if ok.len() > 6 {
            format!(" and {} more", ok.len() - 6)
        } else {
            String::new()
        };
        v.push(format!("only for {}{more}", shown.join(", ")));
    }
    if !bad.is_empty() {
        v.push(format!("not for {}", bad.join(", ")));
    }
    Some(format!("{} (this system: {sys})", v.join("; ")))
}

/// First "Known issues" bullet, shortened.
fn insecure_reason(rest: &str) -> Option<String> {
    let issues = rest.split("Known issues:").nth(1)?;
    let issues = issues.split("You can install it anyway").next()?;
    let first = issues.trim().trim_start_matches("- ").split("\n\n").next()?.trim();
    let one: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
    // keep whole sentences, up to ~200 characters
    let short = match one.char_indices().nth(200) {
        None => one,
        Some((i, _)) => match one[..i].rfind(". ") {
            Some(end) => one[..=end].to_string(),
            None => format!("{}…", &one[..i]),
        },
    };
    Some(wrap(&short, 70))
}

impl Kind {
    fn code(&self) -> String {
        match self {
            Kind::Insecure => "meta/insecure".into(),
            Kind::Unfree(_) => "meta/unfree".into(),
            Kind::Broken => "meta/broken".into(),
            Kind::Unsupported => "meta/unsupported".into(),
            Kind::Blocklisted(_) => "meta/blocklisted".into(),
            Kind::NonSource => "meta/non-source".into(),
            Kind::Problem(p) => format!("meta/{p}"),
        }
    }
    fn adjective(&self) -> String {
        match self {
            Kind::Insecure => "is marked as insecure".into(),
            Kind::Unfree(_) => "has an unfree license".into(),
            Kind::Broken => "is marked as broken".into(),
            Kind::Unsupported => "is not available on this platform".into(),
            Kind::Blocklisted(_) => "has a blocklisted license".into(),
            Kind::NonSource => "contains binaries not built from source".into(),
            Kind::Problem(p) => format!("has a problem ({p})"),
        }
    }
    fn short(&self) -> &str {
        match self {
            Kind::Insecure => "insecure",
            Kind::Unfree(_) => "unfree",
            Kind::Broken => "broken",
            Kind::Unsupported => "unsupported",
            Kind::Blocklisted(_) => "blocklisted",
            Kind::NonSource => "non-source",
            Kind::Problem(_) => "problem",
        }
    }
}

fn nix_ident(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '\'')
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        s.to_string()
    } else {
        format!("\"{s}\"")
    }
}

/// The permit fix, as NixOS module code or as an env var for plain `nix`.
fn permit_fix(m: &MetaError, nixos: bool) -> Fix {
    let pn = pname(&m.package);
    let (title, nixos_code, env) = match &m.kind {
        Kind::Insecure => (
            format!("permit {}", m.package),
            format!("nixpkgs.config.permittedInsecurePackages = [ \"{}\" ];", m.package),
            "NIXPKGS_ALLOW_INSECURE",
        ),
        Kind::Unfree(_) => (
            format!("allow the unfree package {pn}"),
            format!("nixpkgs.config.allowUnfreePredicate = pkg:\n  builtins.elem (lib.getName pkg) [ \"{pn}\" ];"),
            "NIXPKGS_ALLOW_UNFREE",
        ),
        Kind::Broken => (
            format!("try to build {pn} anyway"),
            format!("nixpkgs.config.problems.handlers.{}.broken = \"warn\";", nix_ident(&pn)),
            "NIXPKGS_ALLOW_BROKEN",
        ),
        Kind::Unsupported => (
            "only if you know it works on this platform, allow it anyway".into(),
            "nixpkgs.config.allowUnsupportedSystem = true;".into(),
            "NIXPKGS_ALLOW_UNSUPPORTED_SYSTEM",
        ),
        Kind::NonSource => (
            format!("allow binary sources only for {pn}"),
            format!("nixpkgs.config.allowNonSourcePredicate = pkg:\n  builtins.elem (lib.getName pkg) [ \"{pn}\" ];"),
            "NIXPKGS_ALLOW_NONSOURCE",
        ),
        Kind::Blocklisted(_) | Kind::Problem(_) => {
            return Fix::text("see the full Nix message (--raw) for how to allow it");
        }
    };
    if nixos {
        Fix::code(title, nixos_code)
    } else {
        Fix::code(
            format!("{title} for one invocation"),
            format!("{env}=1 nix build --impure …"),
        )
    }
}

pub fn diagnose(ctx: &Ctx, e: &NixError) -> Diagnostic {
    let Some(m) = parse(&e.message) else {
        return super::eval::diagnose(ctx, e);
    };
    let mut d = Diagnostic::new(&m.kind.code(), format!("{} {}", m.package, m.kind.adjective()));
    if let Some(r) = &m.reason {
        d.note("reason", r.clone());
    }
    let nixos = matches!(ctx.target, Target::Nixos { .. } | Target::Home { .. });
    let pn = pname(&m.package);
    if m.kind == Kind::NonSource && pn.contains("bootstrap") {
        d.note(
            "note",
            wrap(
                &format!(
                    "{pn} is a prebuilt compiler that nixpkgs needs to build the real one. \
                     With allowNonSource = false it is rejected in almost every system, so allow it explicitly."
                ),
                70,
            ),
        );
    }
    let mut fixes = vec![];
    if m.kind == Kind::Broken
        && let Some(at) = mark_user_broken(ctx, &mut d, &pn)
    {
        fixes.push(Fix::text(format!(
            "remove `broken = true` at {at} if the package builds again"
        )));
    }

    let mut short_via = String::new();

    if ctx.no_rerun {
        d.note("note", "diagnostic re-run skipped; dependents unknown");
    } else {
        match dependents(ctx, &m.package) {
            Ok(mut entries) => {
                graph::rank(&mut entries, ctx.own);
                render_entries(ctx, &mut d, &m, &mut entries, &mut fixes, &mut short_via);
            }
            Err(err) => {
                d.note(
                    "note",
                    format!("could not find what depends on {pn}:\n{}", err.root_cause()),
                );
            }
        }
    }
    fixes.push(permit_fix(&m, nixos));
    d.fixes(fixes);
    d.short = format!("{pn} {}{short_via}", m.kind.short());
    d
}

/// If the user's own code sets `meta.broken = true` (an overlay copied from
/// an issue, a forgotten override), show it: the fix is then to remove it.
/// Only bindings below an attribute named like the package count.
fn mark_user_broken(ctx: &Ctx, d: &mut Diagnostic, pn: &str) -> Option<String> {
    // overlays are often in files that are not modules: modules first
    let mut files = ctx.own.module_files();
    files.extend(
        ctx.own
            .user_nix_files()
            .into_iter()
            .filter(|f| !files.contains(f))
            .collect::<Vec<_>>(),
    );
    for path in files {
        let Some(f) = nixast::File::load(&path) else { continue };
        let hit = f.defs().into_iter().find(|def| {
            def.last_seg() == "broken"
                && def.value.as_ref().is_some_and(|v| v.text() == "true")
                && def.path.iter().any(|s| s.name == pn)
        });
        if let Some(def) = hit {
            let start = def.path[def.path.len() - def.own].range.start();
            let end = def.value.as_ref().unwrap().text_range().end();
            let span = f.span(rnix::TextRange::new(start, end));
            let display = crate::render::display_path(&path);
            d.snippet(Snippet {
                title: Some(format!("{pn} is marked broken in your code")),
                display: display.clone(),
                path,
                labels: vec![Label::primary(span.line, span.column, span.len, "set here")],
            });
            return Some(format!("{display}:{}", span.line));
        }
    }
    None
}

fn dependents(ctx: &Ctx, package: &str) -> anyhow::Result<Vec<graph::Entry>> {
    let sys = graph::evaluate(ctx.target)?;
    let g = DrvGraph::load(&sys.toplevel_drv)?;
    let targets = g.find_by_name(package);
    anyhow::ensure!(!targets.is_empty(), "{package} is not in the derivation closure");
    let entries = graph::entries(&g, &targets, &sys.provenance);
    if std::env::var_os("NIXPLAIN_DEBUG").is_some() {
        for e in &entries {
            eprintln!("entry {}: {:?} added_by={:?}", e.name, e.chain, e.added_by);
        }
    }
    Ok(entries)
}

fn render_entries(
    ctx: &Ctx,
    d: &mut Diagnostic,
    m: &MetaError,
    entries: &mut [graph::Entry],
    fixes: &mut Vec<Fix>,
    short_via: &mut String,
) {
    let Some(first) = entries.first() else { return };
    let pn = pname(&m.package);
    d.section("pulled in by", graph::tree(&first.chain));

    if !matches!(ctx.target, Target::Nixos { .. } | Target::Home { .. }) {
        // A plain package: list the direct dependencies that lead there.
        let root = pname(&first.chain[0]);
        let mut via: Vec<String> = entries
            .iter()
            .filter_map(|e| e.chain.get(1))
            .map(|n| pname(n))
            .collect();
        via.sort();
        via.dedup();
        let direct = via.contains(&pn);
        via.retain(|v| *v != pn);
        let mut lines = vec![];
        if direct {
            lines.push(format!("{root} depends on {pn} directly"));
        }
        lines.extend(via.iter().map(|v| format!("{root} → {v} → … → {pn}")));
        if lines.len() > 1 {
            d.section(format!("{root} reaches {pn} in {} ways", lines.len()), lines);
        }
        *short_via = format!(" ← {root}");
        return;
    }

    let mut rows: Vec<Row> = vec![];
    for e in entries.iter() {
        let row = describe(ctx, e, &pn);
        if !rows.iter().any(|r| r.name == row.name || r.via == row.via) {
            rows.push(row);
        }
    }

    if rows.len() > 1 {
        // The user's own entries in full; what NixOS itself adds in one line
        // when there is a lot of it (e.g. a bootstrap compiler).
        let (nixos_rows, own_rows): (Vec<&Row>, Vec<&Row>) = rows.iter().partition(|r| r.nixos);
        let collapse = nixos_rows.len() > 3;
        let shown: Vec<&Row> = if collapse {
            own_rows.clone()
        } else {
            rows.iter().collect()
        };
        let shown = &shown[..shown.len().min(10)];
        let width = shown.iter().map(|r| r.name.len()).max().unwrap_or(0);
        let mut lines: Vec<String> = shown
            .iter()
            .map(|r| format!("{:<width$}  ← {}", r.name, r.via))
            .collect();
        if collapse {
            let names: Vec<&str> = nixos_rows.iter().take(4).map(|r| r.name.as_str()).collect();
            lines.push(format!(
                "and {} packages NixOS itself adds ({}, …)",
                nixos_rows.len(),
                names.join(", ")
            ));
        } else if rows.len() > shown.len() {
            lines.push(format!("… and {} more", rows.len() - shown.len()));
        }
        d.section(format!("{pn} is needed by {} packages", rows.len()), lines);
    }

    // Point at the user's own lines.
    for r in rows.iter().filter(|r| r.snippet.is_some()).take(3) {
        let s = r.snippet.clone().unwrap();
        if short_via.is_empty() {
            let line = s.labels[0].line;
            *short_via = format!(" ← {} ← {}:{line}", r.name, s.display);
        }
        d.snippet(s);
    }
    if short_via.is_empty() {
        *short_via = format!(" ← {}", rows[0].name);
    }
    for r in &rows {
        if let Some(f) = &r.removal
            && !fixes.iter().any(|x| &x.title == f)
        {
            fixes.push(Fix::text(f.clone()));
        }
    }
}

/// One line of "X is needed by": who, through which option, where.
struct Row {
    /// Added by a NixOS module in nixpkgs, not by the user.
    nixos: bool,
    name: String,
    via: String,
    snippet: Option<Snippet>,
    removal: Option<String>,
}

fn describe(ctx: &Ctx, e: &graph::Entry, target_pname: &str) -> Row {
    // Added through an option we know (environment.systemPackages, users.users.*.packages)
    let def = e
        .added_by
        .iter()
        .find(|(_, f)| ctx.own.is_user(f))
        .or(e.added_by.first());
    if let Some((opt, file)) = def {
        let name = pname(&e.name);
        let loc = ctx.own.locate(file);
        let mut row = Row {
            nixos: loc.owner != Owner::User,
            name: name.clone(),
            via: String::new(),
            snippet: None,
            removal: None,
        };
        let mut at = String::new();
        let mut shown_file = loc.display.clone();
        if loc.owner == Owner::User
            && let Some(path) = loc.path.clone()
        {
            // the element in the option's list(s): `pkgs.nheko`, `with pkgs; [ nheko ]`.
            // Nix attributes a module imported by value (`users.alice = import
            // ./home.nix`) to the importing file, so if the package is not
            // there, look in the other files that define the option.
            let find = |p: &std::path::Path| {
                nixast::File::load(p).and_then(|f| {
                    nixast::list_items(&f, opt, 2)
                        .into_iter()
                        .find(|(n, _, _)| *n == name || name.ends_with(&format!("-{n}")))
                        .map(|i| i.1)
                })
            };
            let found = find(&path)
                .map(|sp| (path.clone(), loc.display.clone(), sp))
                .or_else(|| {
                    ctx.own
                        .user_nix_files()
                        .into_iter()
                        .filter(|f| *f != path)
                        .find_map(|f| {
                            let sp = find(&f)?;
                            Some((f.clone(), crate::render::display_path(&f), sp))
                        })
                });
            if let Some((file, display, sp)) = found {
                at = format!(":{}", sp.line);
                shown_file = display.clone();
                row.snippet = Some(Snippet {
                    title: Some(format!("{name} comes from your config")),
                    display,
                    path: file,
                    labels: vec![Label::primary(sp.line, sp.column, sp.len, format!("in {opt}"))],
                });
            }
            row.removal = Some(format!("remove {name}"));
        }
        let module = if loc.owner == Owner::User {
            ""
        } else {
            "  (NixOS module)"
        };
        row.via = format!("{opt:<26}  {shown_file}{at}{module}");
        return row;
    }

    // Added by a module some other way, typically a systemd service.
    let chain = &e.chain[1..];
    let unit = chain
        .iter()
        .find_map(|n| n.strip_prefix("unit-").and_then(|u| u.strip_suffix(".service")))
        .map(String::from);
    let package = chain
        .iter()
        .find(|n| split_name_version(n).1.is_some() && pname(n) != target_pname && !n.starts_with("unit-"))
        .map(|n| pname(n));
    let Some(unit) = unit else {
        let hop = chain.first().cloned().unwrap_or_default();
        let name = package.unwrap_or_else(|| hop.clone());
        return Row {
            nixos: true,
            name,
            via: format!("via {hop} (added by a NixOS module)"),
            snippet: None,
            removal: None,
        };
    };
    let name = package.unwrap_or_else(|| format!("{unit}.service"));
    let mut row = Row {
        nixos: true,
        name: name.clone(),
        via: format!("systemd.services.{unit}"),
        snippet: None,
        removal: None,
    };

    // Find where the user enables the service. Unit names often extend the
    // option name (`pantalaimon-matrix` for services.pantalaimon-headless
    // .instances.matrix), so try ever shorter prefixes.
    let parts: Vec<&str> = unit.split('-').collect();
    for n in (1..=parts.len()).rev() {
        let prefix = parts[..n].join("-");
        if prefix.len() < 3 {
            break;
        }
        for path in ctx.own.module_files() {
            let Some(f) = nixast::File::load(&path) else { continue };
            let mut hits: Vec<(nixast::Span, String)> = f
                .defs()
                .into_iter()
                .filter(|d| !d.is_let)
                .filter_map(|d| {
                    let p = d.option_path();
                    if p.len() < 2 || p[0] != "services" || !p[1].starts_with(&prefix) {
                        return None;
                    }
                    let off = d.path.len() - p.len();
                    let r = d.segs_range(off, off + 2)?;
                    Some((f.span(r), p[1].to_string()))
                })
                .collect();
            hits.sort_by_key(|h| (h.0.line, h.0.column));
            if let Some((sp, svc)) = hits.into_iter().next() {
                let display = crate::render::display_path(&path);
                row.via = format!("services.{svc}  {display}:{}  (NixOS module)", sp.line);
                row.snippet = Some(Snippet {
                    title: Some(format!("{name} comes from a service you enabled")),
                    display,
                    path: path.clone(),
                    labels: vec![Label::primary(
                        sp.line,
                        sp.column,
                        sp.len,
                        format!("this service needs {target_pname}"),
                    )],
                });
                row.removal = Some(format!("disable services.{svc}"));
                row.nixos = false;
                return row;
            }
        }
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nixlog::parse_str;

    #[test]
    fn parses_insecure() {
        let st = parse_str(include_str!("../../tests/fixtures/insecure.jsonl"));
        let m = parse(&st.errors[0].message).unwrap();
        assert_eq!(m.package, "olm-3.2.16");
        assert_eq!(m.kind, Kind::Insecure);
        assert!(
            m.reason
                .unwrap()
                .starts_with("The libolm end‐to‐end encryption library")
        );
    }

    #[test]
    fn parses_unfree() {
        let st = parse_str(include_str!("../../tests/fixtures/unfree.jsonl"));
        let m = parse(&st.errors[0].message).unwrap();
        assert_eq!(m.package, "steam-unwrapped-1.0.0.87");
        assert_eq!(m.kind, Kind::Unfree("unfreeRedistributable".into()));
    }

    #[test]
    fn parses_broken() {
        let msg = "Refusing to evaluate package 'brk-1.0' in «unknown-file» because it has problems:\n- broken: This package is broken.\n\nSee also";
        let m = parse(msg).unwrap();
        assert_eq!(m.kind, Kind::Broken);
        assert_eq!(m.reason.as_deref(), Some("This package is broken."));
    }
}
