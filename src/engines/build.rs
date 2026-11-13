//! Class D: a builder exited non-zero.
//!
//! Deliberately generic: no per-build-system knowledge. What helps in every
//! case is finding the right lines in the log, knowing whether the package
//! is the user's own (or overridden by them), and for nixpkgs packages what
//! changed since the last build that worked.
//!
//! 1. full log + failing phase (from the JSON events, else `nix log`)
//! 2. the first real error line with its context, instead of the last lines
//! 3. ownership: own package, own override, or plain nixpkgs
//! 4. diff against the same package in the last good generation

use super::Ctx;
use crate::context::{Target, last_good_system_drv};
use crate::diag::{Diagnostic, Fix, Label, Snippet};
use crate::graph::{self, DrvGraph};
use crate::nixast;
use crate::nixlog::NixError;
use crate::util::{drv_name, nix, pname, run, split_name_version};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::process::Command;
use std::sync::LazyLock;

static FAILED_DRV: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:Cannot build|builder for) '(/nix/store/[^']+\.drv)'").unwrap());
static EXIT_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"exit code (\d+)").unwrap());
static NO_OUTPUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"failed to produce output path for output '([^']+)'").unwrap());
static RUNNING_PHASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Running phase: (\w+)").unwrap());
static GENERIC_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\berror\b(\[|:| )|\bfatal\b|\bfailed\b|undefined reference|no such file or directory|not found|traceback \(most recent call last\)|\bpanicked at\b|segmentation fault)",
    )
    .unwrap()
});
static NOT_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(-W(no-)?error|\berror\.[ch]\b|\b0 (errors?|failed)\b|errors?[=:] ?0\b|^\s*warning|-- Looking for|checking for|^Running phase|^@nix )",
    )
    .unwrap()
});

/// The first line that looks like a real error.
pub fn first_error(log: &[String]) -> Option<usize> {
    log.iter().position(|l| is_error(l))
}

fn is_error(l: &str) -> bool {
    GENERIC_ERROR.is_match(l) && !NOT_ERROR.is_match(l)
}

/// The error line plus its context: a preceding "In function …:" style line
/// and following continuation lines (compiler excerpts, indented details).
pub fn excerpt(log: &[String], i: usize) -> Vec<String> {
    let mut out = vec![];
    if i > 0 && log[i - 1].trim_end().ends_with(':') && !is_error(&log[i - 1]) {
        out.push(log[i - 1].clone());
    }
    out.push(log[i].clone());
    for l in log.iter().skip(i + 1).take(6) {
        let cont = l.starts_with(' ') || l.starts_with('\t') || l.trim_start().starts_with('|');
        if !cont {
            break;
        }
        out.push(l.clone());
    }
    out
}

fn phase_at(log: &[String], upto: usize) -> Option<String> {
    log[..=upto.min(log.len().saturating_sub(1))]
        .iter()
        .rev()
        .find_map(|l| RUNNING_PHASE.captures(l).map(|c| c[1].to_string()))
}

// ---- engine ----------------------------------------------------------------

pub fn diagnose(ctx: &Ctx, e: &NixError) -> Diagnostic {
    let Some(drv) = FAILED_DRV.captures(&e.message).map(|c| c[1].to_string()) else {
        return super::eval::diagnose(ctx, e);
    };
    let name = drv_name(&drv);
    let pn = pname(&name);
    let exit = EXIT_CODE.captures(&e.message).map(|c| c[1].to_string());

    // 1. log + phase
    let act = ctx.log.build_for(&drv);
    let mut log: Vec<String> = act.map(|a| a.log.clone()).unwrap_or_default();
    if log.len() < 30
        && !ctx.no_rerun
        && let Ok(full) = run(nix().args(["log", &drv]))
    {
        let full: Vec<String> = full
            .lines()
            .filter(|l| !l.starts_with("@nix "))
            .map(crate::util::strip_ansi)
            .collect();
        if full.len() > log.len() {
            log = full;
        }
    }
    if log.is_empty() {
        // Fall back to the "Last N log lines" Nix put into the message.
        log = e
            .message
            .lines()
            .filter_map(|l| l.strip_prefix("> "))
            .map(String::from)
            .collect();
    }
    let event_phase = act.and_then(|a| a.phase.clone());
    let no_output = NO_OUTPUT.captures(&e.message).map(|c| c[1].to_string());

    // 2. first real error (not for a missing output: the log has no error then)
    let err_line = if no_output.is_some() { None } else { first_error(&log) };
    let phase = event_phase
        .clone()
        .or_else(|| phase_at(&log, err_line.unwrap_or(log.len().saturating_sub(1))));

    let code = match phase.as_deref() {
        Some("configurePhase") => "build/configure",
        Some("buildPhase") => "build/compile",
        Some(p) if p.ends_with("CheckPhase") || p == "checkPhase" => "build/check",
        Some("installPhase") | Some("fixupPhase") => "build/install",
        _ => "build/failed",
    };
    let in_phase = phase.as_deref().map(|p| format!(" in {p}")).unwrap_or_default();
    let mut d = match &no_output {
        Some(out) => {
            let mut d = Diagnostic::new(
                "build/no-output",
                format!("{name} built, but did not create its output `{out}`"),
            );
            d.short = format!("{name} did not create ${out}");
            d
        }
        None => {
            let mut d = Diagnostic::new(code, format!("{name} fails{in_phase}"));
            d.short = format!("{name} fails{in_phase}");
            d
        }
    };

    match err_line {
        Some(i) => {
            let more = log.iter().skip(i + 1).filter(|l| is_error(l)).count();
            let title = if more > 0 {
                let lines = if more == 1 { "line" } else { "lines" };
                format!(
                    "first error (log line {}/{}, {more} more error {lines} after it)",
                    i + 1,
                    log.len()
                )
            } else {
                format!("error (log line {}/{})", i + 1, log.len())
            };
            d.section(title, excerpt(&log, i));
        }
        None => {
            let tail: Vec<String> = log.iter().rev().take(6).rev().cloned().collect();
            let title = match (&no_output, &exit) {
                (Some(_), _) => "last lines of the log".to_string(),
                (None, Some(c)) => format!("the log has no error message; its last lines (exit code {c})"),
                (None, None) => "the log has no error message; its last lines".to_string(),
            };
            d.section(title, tail);
        }
    }
    if let Some(out) = &no_output {
        d.note(
            "why",
            format!("every output must exist when the build ends, but nothing was written to ${out}"),
        );
    }

    // 3. ownership
    let owned = find_definition(ctx, &pn);
    if let Some((path, display, line, col, len)) = &owned {
        d.snippet(Snippet {
            title: Some("the package is defined in your code".into()),
            display: display.clone(),
            path: path.clone(),
            labels: vec![Label::primary(*line, *col, *len, "defined here")],
        });
    }
    let overridden = if owned.is_none() { find_override(ctx, &pn) } else { None };
    if let Some((path, display, line, col, len)) = &overridden {
        d.snippet(Snippet {
            title: Some(format!("{pn} comes from nixpkgs, but you override it")),
            display: display.clone(),
            path: path.clone(),
            labels: vec![Label::primary(*line, *col, *len, "your override; check it first")],
        });
    } else if owned.is_none() {
        d.note(
            "note",
            "this package comes from nixpkgs or a flake input, not from your code",
        );
    }

    // 4. last good generation, dependency chain
    let mut compiler_changed = false;
    if matches!(ctx.target, Target::Nixos { .. }) && !ctx.no_rerun {
        if let Some((generation, old_sys)) = last_good_system_drv() {
            match baseline_diff(&old_sys, &drv) {
                Ok(Some(diff)) => {
                    compiler_changed = diff.iter().any(|l| {
                        [
                            "gcc", "clang", "rustc", "go ", "python3 ", "cmake", "meson", "glibc", "binutils",
                        ]
                        .iter()
                        .any(|c| l.trim_start().starts_with(c))
                    });
                    d.section(format!("changed since your last good generation (#{generation})"), diff);
                }
                Ok(None) => {
                    d.note(
                        "note",
                        format!("{pn} was not part of your last good generation (#{generation})"),
                    );
                }
                Err(_) => {}
            }
        }
        if let Ok(chain) = pulled_in_by(ctx, &drv) {
            d.section("pulled in by", chain);
        }
    }

    // fixes: only where there is something concrete to say
    let mut fixes = vec![];
    if no_output.is_some() {
        fixes.push(Fix::code(
            "make the install step copy the result to $out, e.g.",
            "installPhase = ''\n  runHook preInstall\n  mkdir -p $out/bin\n  cp <program> $out/bin/\n  runHook postInstall\n'';",
        ));
    }
    if overridden.is_some() {
        fixes.push(Fix::text(format!(
            "build it without your override to see whether the override breaks it: nix build nixpkgs#{pn}"
        )));
    }
    if owned.is_none() && overridden.is_none() {
        if compiler_changed {
            fixes.push(Fix::text(
                "the toolchain changed since your last good build; this is likely an upstream breakage",
            ));
        }
        fixes.push(Fix::text(format!(
            "check whether it also fails on Hydra: https://hydra.nixos.org/search?query={pn}"
        )));
        if code == "build/check" {
            fixes.push(Fix::code(
                "skip its tests for now",
                format!("nixpkgs.overlays = [ (final: prev: {{\n  {pn} = prev.{pn}.overrideAttrs {{ doCheck = false; }};\n}}) ];"),
            ));
        }
        fixes.push(Fix::text(
            "take the package from your previous nixpkgs revision for now, or remove it",
        ));
    }
    d.fixes(fixes);
    d.note("full log", format!("nix log {drv}"));
    d
}

type Found = (std::path::PathBuf, String, usize, usize, usize);

/// A package defined in the user's code: `pname = "<pn>"`, or
/// `name = "<pn>-…"`. Package files are usually not modules (callPackage), so
/// this searches the whole flake.
fn find_definition(ctx: &Ctx, pn: &str) -> Option<Found> {
    for path in ctx.own.user_nix_files() {
        let Some(f) = nixast::File::load(&path) else { continue };
        for def in f.defs() {
            let key = def.last_seg();
            let Some(v) = def.value.as_ref().filter(|v| v.kind() == rnix::SyntaxKind::NODE_STRING) else {
                continue;
            };
            // `"libfoo"` or `"libfoo-${version}"`: the literal start of the string
            let text = v.text().to_string();
            let content = text.trim_start_matches('"');
            let hit = match key {
                "pname" => content.strip_prefix(pn).is_some_and(|r| r.starts_with('"')),
                "name" => content.strip_prefix(pn).is_some_and(|r| r.starts_with('-')),
                _ => false,
            };
            if hit {
                let start = v.text_range().start() + rnix::TextSize::from(1);
                let sp = f.span(rnix::TextRange::at(start, rnix::TextSize::from(pn.len() as u32)));
                return Some((
                    path.clone(),
                    crate::render::display_path(&path),
                    sp.line,
                    sp.column,
                    sp.len,
                ));
            }
        }
    }
    None
}

/// `pn = prev.pn.overrideAttrs …` and friends in the user's overlays.
fn find_override(ctx: &Ctx, pn: &str) -> Option<Found> {
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
        for def in f.defs() {
            if def.last_seg() != pn {
                continue;
            }
            let Some(v) = &def.value else { continue };
            let text = v.text().to_string();
            let text = text.trim_start_matches('(').trim_start();
            if ["prev", "super", "old", "pkgs", "final", "self"]
                .iter()
                .any(|b| text.starts_with(&format!("{b}.{pn}")))
            {
                let sp = f.span(def.path.last().unwrap().range);
                return Some((
                    path.clone(),
                    crate::render::display_path(&path),
                    sp.line,
                    sp.column,
                    sp.len,
                ));
            }
        }
    }
    None
}

fn pulled_in_by(ctx: &Ctx, drv: &str) -> anyhow::Result<Vec<String>> {
    let sys = graph::evaluate(ctx.target)?;
    let g = DrvGraph::load(&sys.toplevel_drv)?;
    let base = drv.rsplit('/').next().unwrap_or(drv).to_string();
    let mut entries = graph::entries(&g, &[base], &sys.provenance);
    graph::rank(&mut entries, ctx.own);
    let e = entries.first().ok_or_else(|| anyhow::anyhow!("not reachable"))?;
    let mut lines = graph::tree(&e.chain);
    if let Some((opt, file)) = e.added_by.first() {
        lines.push(format!(
            "({} ← {opt} in {})",
            pname(&e.name),
            ctx.own.locate(file).display
        ));
    }
    Ok(lines)
}

// ---- last good generation --------------------------------------------------

/// Input drv names of a set of derivations, from `nix derivation show`
/// (handles the 2.33+ `{"derivations": …}` and the older format).
fn input_names(drvs: &[String]) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let out = run(nix().arg("derivation").arg("show").args(drvs))?;
    let v: serde_json::Value = serde_json::from_str(&out)?;
    let map = v
        .get("derivations")
        .unwrap_or(&v)
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut res = HashMap::new();
    for (k, d) in map {
        let mut inputs: Vec<String> = d
            .pointer("/inputs/drvs")
            .or_else(|| d.get("inputDrvs"))
            .and_then(|x| x.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        // stdenv: the toolchain (cc wrapper, coreutils, …) is listed in its env
        if k.ends_with("-stdenv-linux.drv") || k.ends_with("-stdenv-darwin.drv") {
            inputs.clear();
            for var in ["defaultNativeBuildInputs", "initialPath"] {
                if let Some(paths) = d.pointer(&format!("/env/{var}")).and_then(|x| x.as_str()) {
                    inputs.extend(paths.split_whitespace().map(String::from));
                }
            }
        }
        res.insert(k.rsplit('/').next().unwrap_or(&k).to_string(), inputs);
    }
    Ok(res)
}

fn versions(names: &[String]) -> BTreeMap<String, BTreeSet<String>> {
    let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for n in names {
        let (p, v) = split_name_version(&drv_name(n));
        m.entry(p).or_default().insert(v.unwrap_or_default());
    }
    m
}

/// Compare the failing drv with the same package in the last good system.
/// Looks at direct inputs and one level below unversioned ones (stdenv,
/// hooks), which is where the compiler lives.
fn baseline_diff(old_system_drv: &str, new_drv: &str) -> anyhow::Result<Option<Vec<String>>> {
    let new_name = drv_name(new_drv);
    let pn = pname(&new_name);
    let closure = run(Command::new("nix-store").args(["--query", "--requisites", old_system_drv]))?;
    let mut cands: Vec<&str> = closure
        .lines()
        .filter(|l| l.ends_with(".drv") && pname(&drv_name(l)) == pn)
        .collect();
    cands.sort_by_key(|c| drv_name(c) != new_name);
    let Some(old_drv) = cands.first().map(|s| s.to_string()) else {
        return Ok(None);
    };
    if old_drv == new_drv {
        return Ok(Some(vec![
            "nothing: this exact derivation built before (non-deterministic failure?)".into(),
        ]));
    }

    let level1 = input_names(&[old_drv.clone(), new_drv.to_string()])?;
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let mut old_in = level1.get(&base(&old_drv)).cloned().unwrap_or_default();
    let mut new_in = level1.get(&base(new_drv)).cloned().unwrap_or_default();

    // one level deeper into stdenv, which is where the compiler lives
    let unversioned: Vec<String> = old_in
        .iter()
        .chain(new_in.iter())
        .filter(|n| drv_name(n).starts_with("stdenv-"))
        .map(|n| {
            if n.starts_with('/') {
                n.clone()
            } else {
                format!("/nix/store/{n}")
            }
        })
        .collect();
    if !unversioned.is_empty()
        && let Ok(level2) = input_names(&unversioned)
    {
        for n in old_in.clone() {
            if let Some(sub) = level2.get(&base(&n)) {
                old_in.extend(sub.iter().cloned());
            }
        }
        for n in new_in.clone() {
            if let Some(sub) = level2.get(&base(&n)) {
                new_in.extend(sub.iter().cloned());
            }
        }
    }

    let (ov, nv) = (versions(&old_in), versions(&new_in));
    let mut lines = vec![];
    let (_, own_old) = split_name_version(&drv_name(&old_drv));
    let (_, own_new) = split_name_version(&new_name);
    let width = ov
        .keys()
        .chain(nv.keys())
        .map(|k| k.len())
        .max()
        .unwrap_or(0)
        .max(pn.len())
        .min(30);
    if own_old != own_new {
        lines.push(format!(
            "{pn:<width$}  {} → {}",
            own_old.unwrap_or_default(),
            own_new.unwrap_or_default()
        ));
    } else {
        lines.push(format!("{pn:<width$}  unchanged ({})", own_new.unwrap_or_default()));
    }
    for (p, nvs) in &nv {
        let Some(ovs) = ov.get(p) else { continue };
        if ovs != nvs && !(ovs.len() == 1 && ovs.contains("")) {
            let fmt = |s: &BTreeSet<String>| s.iter().cloned().collect::<Vec<_>>().join(", ");
            lines.push(format!("{p:<width$}  {} → {}", fmt(ovs), fmt(nvs)));
        }
    }
    let fmt_one = |m: &BTreeMap<String, BTreeSet<String>>, p: &str| {
        let v: Vec<String> = m[p].iter().filter(|v| !v.is_empty()).cloned().collect();
        if v.is_empty() {
            p.to_string()
        } else {
            format!("{p} {}", v.join(", "))
        }
    };
    let versioned = |m: &BTreeMap<String, BTreeSet<String>>, p: &str| m[p].iter().any(|v| !v.is_empty());
    for p in ov.keys().filter(|p| !nv.contains_key(*p) && versioned(&ov, p)) {
        lines.push(format!("{:<width$}  removed", fmt_one(&ov, p)));
    }
    for p in nv.keys().filter(|p| !ov.contains_key(*p) && versioned(&nv, p)) {
        lines.push(format!("{:<width$}  added", fmt_one(&nv, p)));
    }
    if lines.len() == 1 {
        lines.push("no direct input changed its version (the change is deeper or in the recipe)".into());
    }
    lines.truncate(10);
    Ok(Some(lines))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn first_error_skips_noise() {
        let log = l(
            "gcc -Werror -c a.c\nchecking for foo... not found\nok\nsrc/ext.c:88:5: error: something bad\nmake: *** [Makefile:3] Error 1",
        );
        assert_eq!(first_error(&log), Some(3));
    }

    #[test]
    fn excerpt_with_context() {
        // real GCC output from the demo
        let log = l(
            "Running phase: buildPhase\nold.c: In function 'main':\nold.c:2:10: error: implicit declaration of function 'legacy_init'\n    2 |   return legacy_init(42);\n      |          ^~~~~~~~~~~\nmake: failed",
        );
        let i = first_error(&log).unwrap();
        assert_eq!(excerpt(&log, i).len(), 4);
        assert_eq!(excerpt(&log, i)[0], "old.c: In function 'main':");
    }

    #[test]
    fn fixture_log() {
        let st = crate::nixlog::parse_str(include_str!("../../tests/fixtures/build.jsonl"));
        let log = &st.builds.values().next().unwrap().log;
        assert_eq!(first_error(log), Some(1));
    }
}

#[cfg(test)]
mod system_tests {
    /// Needs a NixOS system profile and the nixpkgs used in development.
    /// Run with: NIXPLAIN_TEST_DRV=$(nix eval --raw nixpkgs#bashInteractive.drvPath) cargo test -- --ignored
    #[test]
    #[ignore]
    fn baseline_against_real_system() {
        let new = std::env::var("NIXPLAIN_TEST_DRV").expect("set NIXPLAIN_TEST_DRV");
        let (generation, old) = crate::context::last_good_system_drv().expect("system profile");
        let diff = super::baseline_diff(&old, &new)
            .unwrap()
            .expect("package in old system");
        eprintln!("generation {generation}:\n{}", diff.join("\n"));
        assert!(!diff.is_empty());
    }
}
