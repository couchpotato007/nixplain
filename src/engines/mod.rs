//! Classifier and per-class diagnostic engines.

pub mod build;
pub mod eval;
pub mod fetch;
pub mod meta;
pub mod module;

use crate::context::{Ownership, Target};
use crate::diag::{Diagnostic, Label, Snippet};
use crate::nixlog::{LogState, NixError};

pub struct Ctx<'a> {
    pub target: &'a Target,
    pub own: &'a Ownership,
    pub log: &'a LogState,
    /// Skip the diagnostic second run (offline / fast mode).
    pub no_rerun: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A: insecure, unfree, broken, unsupported
    Meta,
    /// B: everything else that fails during evaluation
    Eval,
    /// C: module system
    Module,
    /// D: builder exited non-zero
    Build,
    /// E: hash mismatch, download failure
    Fetch,
}

pub fn classify(e: &NixError) -> Class {
    let m = &e.message;
    if m.starts_with("Refusing to evaluate package") {
        Class::Meta
    } else if m.contains("hash mismatch in fixed-output derivation") {
        Class::Fetch
    } else if m.starts_with("Cannot build '") || m.starts_with("builder for '") {
        Class::Build
    } else if m.starts_with("The option `")
        || m.starts_with("A definition for option `")
        || m.starts_with("Failed assertions:")
    {
        Class::Module
    } else {
        Class::Eval
    }
}

/// Errors that only repeat an earlier, more specific failure.
fn is_followup(e: &NixError) -> bool {
    let m = &e.message;
    m.contains("dependency failed")
        || m.contains("dependencies failed")
        || m.contains("dependencies couldn't be built")
        || (m.starts_with("build of '") && m.contains("failed"))
}

pub fn diagnose_all(ctx: &Ctx) -> Vec<Diagnostic> {
    let mut seen = std::collections::HashSet::new();
    let errors: Vec<&NixError> = ctx
        .log
        .errors
        .iter()
        .filter(|e| seen.insert(e.message.clone()))
        .collect();
    let primary: Vec<&&NixError> = errors.iter().filter(|e| !is_followup(e)).collect();
    let errors: Vec<&NixError> = if primary.is_empty() {
        errors
    } else {
        primary.into_iter().copied().collect()
    };
    errors
        .into_iter()
        .map(|e| match classify(e) {
            Class::Meta => meta::diagnose(ctx, e),
            Class::Fetch => fetch::diagnose(ctx, e),
            Class::Build => build::diagnose(ctx, e),
            Class::Module => module::diagnose(ctx, e),
            Class::Eval => eval::diagnose(ctx, e),
        })
        .collect()
}

// ---- helpers shared by engines -------------------------------------------

/// A single-label snippet for a user file.
pub fn snippet_at(ctx: &Ctx, file: &str, line: usize, col: usize, len: usize, label: &str) -> Option<Snippet> {
    let loc = ctx.own.locate(file);
    let path = loc.path?;
    Some(Snippet {
        title: None,
        display: loc.display,
        path,
        labels: vec![Label::primary(line, col, len, label)],
    })
}

pub fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// Wrap prose at `width` columns.
pub fn wrap(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    for w in s.split_whitespace() {
        if col > 0 && col + 1 + w.len() > width {
            out.push('\n');
            col = 0;
        } else if col > 0 {
            out.push(' ');
            col += 1;
        }
        out.push_str(w);
        col += w.len();
    }
    out
}

/// End-to-end engine tests on real Nix 2.34 messages, with the user's flake
/// mapped to a temporary directory.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Owner, Ownership, Target};
    use crate::nixlog::{LogState, NixError};

    const STORE: &str = "/nix/store/l0306q3g6d0mmq5bsp0zp7a5dh5w3j9n-source";

    fn setup(name: &str, files: &[(&str, &str)]) -> Ownership {
        let dir = std::env::temp_dir().join(format!("nixplain-{name}-{}", std::process::id()));
        for (path, content) in files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        Ownership {
            roots: vec![(STORE.into(), Owner::User)],
            local_root: Some(dir),
            ..Default::default()
        }
    }

    fn run(own: &Ownership, message: &str) -> Diagnostic {
        let log = LogState::default();
        let ctx = Ctx {
            target: &Target::Unknown,
            own,
            log: &log,
            no_rerun: true,
        };
        let e = NixError {
            message: message.into(),
            formatted: String::new(),
            pos: None,
            trace: vec![],
        };
        diagnose_all(&Ctx {
            log: &LogState {
                errors: vec![e],
                ..Default::default()
            },
            ..ctx
        })
        .pop()
        .unwrap()
    }

    #[test]
    fn callpackage_argument() {
        let own = setup(
            "cp",
            &[(
                "pkgs/needs-unknown-arg.nix",
                "# x\n{ stdenv, libfoo }:\nstdenv.mkDerivation { }\n",
            )],
        );
        let d = run(
            &own,
            &format!(
                "evaluation aborted with the following error message: 'lib.customisation.callPackageWith: Function called without required argument \"libfoo\" at {STORE}/pkgs/needs-unknown-arg.nix:2, did you mean \"libaio\", \"libao\" or \"libaom\"?'"
            ),
        );
        assert_eq!(d.code, "eval/callpackage-argument");
        let loc = d.location.as_ref().unwrap();
        assert_eq!((loc.line, loc.column), (2, 11));
        let json = crate::render::json(&[d]);
        assert!(json.contains("did you mean `libaio`, `libao`, `libaom`?"));
    }

    #[test]
    fn assertion_marks_both_lines() {
        let own = setup(
            "as",
            &[(
                "hosts/a.nix",
                "{\n  users.users.bob.isNormalUser = true;\n  users.users.bob.isSystemUser = true;\n}\n",
            )],
        );
        let d = run(
            &own,
            "Failed assertions:\n- Exactly one of users.users.bob.isSystemUser and users.users.bob.isNormalUser must be set.",
        );
        assert_eq!(d.code, "module/assertion");
        let lines: Vec<usize> = d
            .blocks
            .iter()
            .filter_map(|b| match b {
                crate::diag::Block::Snippet(s) => Some(s.labels.iter().map(|l| l.line).collect::<Vec<_>>()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(lines, vec![2, 3]);
    }

    #[test]
    fn list_entry_type_error() {
        let own = setup(
            "ty",
            &[(
                "hosts/t.nix",
                "{\n  networking.firewall.allowedTCPPorts = [ \"22\" 80 ];\n}\n",
            )],
        );
        let d = run(
            &own,
            &format!(
                "A definition for option `networking.firewall.allowedTCPPorts.\"[definition 1-entry 1]\"' is not of type `16 bit unsigned integer; between 0 and 65535 (both inclusive)'. Definition values:\n- In `{STORE}/hosts/t.nix': \"22\""
            ),
        );
        assert_eq!(
            d.title,
            "wrong type in entry 1 of option `networking.firewall.allowedTCPPorts`"
        );
        let loc = d.location.unwrap();
        assert_eq!((loc.line, loc.column), (2, 43));
    }
}

#[cfg(test)]
mod placement_tests {
    //! The primary marker goes where the user has to edit.
    use super::*;
    use crate::context::{Owner, Ownership, Target};
    use crate::nixlog::{Frame, LogState, NixError, Pos};

    const STORE: &str = "/nix/store/l0306q3g6d0mmq5bsp0zp7a5dh5w3j9n-source";
    const HELPER: &str = "let\n  mkUser = { name, uid }: {\n    ${name}.uid = uid;\n  };\nin\n{\n  users.users = mkUser {\n    name = \"bob\";\n    uid = 1001;\n    shell = \"/bin/zsh\";\n  };\n}\n";

    fn diag(name: &str, file: &str, src: &str, e: NixError) -> Diagnostic {
        let dir = std::env::temp_dir().join(format!("nixplain-place-{name}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("hosts")).unwrap();
        std::fs::write(dir.join(file), src).unwrap();
        let own = Ownership {
            roots: vec![(STORE.into(), Owner::User)],
            local_root: Some(dir),
            ..Default::default()
        };
        let log = LogState {
            errors: vec![e],
            ..Default::default()
        };
        diagnose_all(&Ctx {
            target: &Target::Unknown,
            own: &own,
            log: &log,
            no_rerun: true,
        })
        .pop()
        .unwrap()
    }

    fn pos(line: usize, column: usize) -> Pos {
        Pos {
            file: format!("{STORE}/hosts/a.nix"),
            line,
            column,
        }
    }

    fn primary(d: &Diagnostic) -> (usize, usize) {
        let l = d.location.as_ref().unwrap();
        (l.line, l.column)
    }

    #[test]
    fn unexpected_argument_points_at_passed_attribute() {
        let e = NixError {
            message: "function 'mkUser' called with unexpected argument 'shell'".into(),
            formatted: String::new(),
            pos: Some(pos(2, 12)),
            trace: vec![Frame {
                pos: Some(pos(7, 17)),
                msg: "from call site".into(),
            }],
        };
        let d = diag("unexp", "hosts/a.nix", HELPER, e);
        assert_eq!(primary(&d), (10, 5)); // `shell = …` in the call
    }

    #[test]
    fn missing_argument_points_at_call() {
        let e = NixError {
            message: "function 'mkUser' called without required argument 'age'".into(),
            formatted: String::new(),
            pos: Some(pos(2, 12)),
            trace: vec![Frame {
                pos: Some(pos(7, 17)),
                msg: "from call site".into(),
            }],
        };
        let d = diag("miss", "hosts/a.nix", HELPER, e);
        assert_eq!(primary(&d), (7, 17)); // `mkUser` in the call, not the definition
    }

    #[test]
    fn syntax_error_points_at_missing_semicolon() {
        let src =
            "{ pkgs, ... }:\n{\n  environment.systemPackages = [ pkgs.hello ]\n  services.openssh.enable = true;\n}\n";
        let e = NixError {
            message: "syntax error, unexpected '=', expecting ';'".into(),
            formatted: String::new(),
            pos: Some(pos(4, 27)),
            trace: vec![],
        };
        let d = diag("syntax", "hosts/a.nix", src, e);
        assert_eq!(d.title, "missing `;` at the end of line 3");
        assert_eq!(primary(&d), (3, 46));
    }
}
