//! Class E: hash mismatches. The correct hash is already in the error; we
//! find the line that holds the old one (in any encoding) and offer an edit.

use super::Ctx;
use crate::diag::{Diagnostic, Edit, Fix, Label, Snippet};
use crate::nixast;
use crate::nixlog::NixError;
use crate::util::{drv_name, nix, run};
use regex::Regex;
use std::sync::LazyLock;

static MISMATCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)hash mismatch in fixed-output derivation '([^']+)':\s*specified:\s*(\S+)\s*got:\s*(\S+)").unwrap()
});

#[derive(Debug, Clone, Copy, PartialEq)]
enum Enc {
    Sri,
    Nix32,
    Base16,
    Base64,
}

/// Convert an SRI hash to another encoding with `nix hash convert`.
fn convert(sri: &str, enc: Enc) -> Option<String> {
    let algo = sri.split_once('-')?.0;
    let to = match enc {
        Enc::Sri => return Some(sri.to_string()),
        Enc::Nix32 => "nix32",
        Enc::Base16 => "base16",
        Enc::Base64 => "base64",
    };
    run(nix().args(["hash", "convert", "--hash-algo", algo, "--to", to, sri]))
        .ok()
        .map(|s| s.trim().to_string())
}

fn is_placeholder(sri: &str) -> bool {
    sri.split_once('-')
        .is_some_and(|(_, h)| h.trim_end_matches('=').chars().all(|c| c == 'A'))
}

pub fn diagnose(ctx: &Ctx, e: &NixError) -> Diagnostic {
    let Some(c) = MISMATCH.captures(&e.message) else {
        let mut d = Diagnostic::new("fetch/error", super::first_line(&e.message));
        d.note("details", e.message.clone());
        return d;
    };
    let (drv, specified, got) = (c[1].to_string(), c[2].to_string(), c[3].to_string());
    let name = drv_name(&drv);
    let placeholder = is_placeholder(&specified);
    let title = if placeholder {
        format!("hash of {name} is still a placeholder")
    } else {
        format!("hash of {name} does not match")
    };
    let mut d = Diagnostic::new("fetch/hash", title);
    d.short = format!("hash mismatch for {name}");

    // Look for the old hash in every encoding a user might have written, and
    // for placeholders also for `lib.fakeHash` & co. written as expressions.
    let mut needles: Vec<(String, Enc)> = [Enc::Sri, Enc::Nix32, Enc::Base16, Enc::Base64]
        .into_iter()
        .filter_map(|enc| convert(&specified, enc).map(|h| (h, enc)))
        .collect();
    if placeholder {
        for fake in [
            "lib.fakeHash",
            "lib.fakeSha256",
            "lib.fakeSha512",
            "fakeHash",
            "fakeSha256",
        ] {
            needles.push((fake.to_string(), Enc::Sri));
        }
    }
    // Candidates from the syntax tree: string literals equal to the old hash,
    // and `hash = lib.fakeHash;` bindings for placeholders.
    struct Cand {
        file: std::path::PathBuf,
        mentions_stem: bool,
        span: nixast::Span,
        old: String,
        enc: Enc,
        attr: Option<String>,
        version: Option<nixast::Span>,
    }
    let stem = crate::util::pname(name.split(".tar").next().unwrap_or(&name));
    let stem = stem.trim_end_matches("-vendor").trim_end_matches("-src").to_string();
    let mut candidates: Vec<Cand> = vec![];
    for path in ctx.own.user_nix_files() {
        let Some(f) = nixast::File::load(&path) else { continue };
        let mentions_stem = f.src.contains(&stem);
        for (old, enc) in &needles {
            let hits: Vec<(nixast::Span, rnix::SyntaxNode)> = if old.contains("fake") {
                f.defs()
                    .into_iter()
                    .filter_map(|d| {
                        d.value
                            .filter(|v| v.text() == old.as_str())
                            .map(|v| (f.span(v.text_range()), v))
                    })
                    .collect()
            } else {
                nixast::find_strings(&f, old)
            };
            for (span, node) in hits {
                let binding = nixast::enclosing_binding(&node);
                candidates.push(Cand {
                    file: path.clone(),
                    mentions_stem,
                    span,
                    old: old.clone(),
                    enc: *enc,
                    attr: binding.as_ref().and_then(nixast::binding_key),
                    version: binding
                        .as_ref()
                        .and_then(|b| nixast::nearby_binding(&f, b, "version"))
                        .map(|v| v.0),
                });
            }
        }
    }
    // A real hash occurs once; placeholders may occur in several files.
    // Prefer the file that mentions the fetched name, then real strings.
    candidates.sort_by_key(|c| (!c.mentions_stem, c.old.contains("fake")));
    let others: Vec<String> = candidates
        .iter()
        .skip(1)
        .map(|c| crate::render::display_path(&c.file))
        .collect();
    let found = candidates.into_iter().next();
    if found.is_some() && !others.is_empty() && !placeholder {
        d.note(
            "note",
            format!("the same old hash also appears in {}", others.join(", ")),
        );
    }

    match found {
        Some(Cand {
            file,
            span,
            old,
            enc,
            attr,
            version,
            ..
        }) => {
            let line = span.line;
            let mut labels = vec![Label::primary(
                span.line,
                span.column,
                span.len,
                if placeholder { "placeholder" } else { "outdated" },
            )];
            // the version of the same package: a version bump is the usual
            // cause of an outdated hash (irrelevant for a placeholder)
            if let Some(v) = version.filter(|_| !placeholder) {
                labels.push(Label::secondary(
                    v.line,
                    v.column,
                    v.len,
                    "the hash must belong to this version",
                ));
            }
            let new = if old.contains("fake") {
                format!("\"{got}\"") // replace the expression by a string
            } else {
                convert(&got, enc).unwrap_or_else(|| got.clone())
            };
            d.snippet(Snippet {
                title: None,
                display: crate::render::display_path(&file),
                path: file.clone(),
                labels,
            });
            // same encoding as in the file; a plain hash for `lib.fakeHash`
            d.note("correct", new.trim_matches('"').to_string());
            if let Some(a) = attr.filter(|a| a != "hash" && a != "sha256") {
                d.note("note", format!("`{a}` is the hash of {}", what_hash(&a)));
            }
            d.edits.push(Edit { file, line, old, new });
            d.fixes(vec![Fix::text("run again with `--fix` to apply the new hash")]);
        }
        None => {
            d.note("specified", specified.clone());
            d.note("got", got.clone());
            d.note(
                "note",
                "this hash is not in your files; it comes from nixpkgs or another flake input",
            );
            d.fixes(vec![
                Fix::text("if you override this source, replace the hash in your override"),
                Fix::text("otherwise the upstream source changed: update the input (`nix flake update`) or report it"),
            ]);
        }
    }
    d
}

fn what_hash(attr: &str) -> &'static str {
    match attr {
        "cargoHash" | "cargoSha256" => "the vendored Cargo dependencies, not the source",
        "vendorHash" | "vendorSha256" => "the vendored Go modules, not the source",
        "npmDepsHash" => "the npm dependencies, not the source",
        "outputHash" => "the derivation output",
        _ => "a fixed-output derivation",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nixlog::parse_str;

    #[test]
    fn parses_mismatch() {
        let st = parse_str(include_str!("../../tests/fixtures/hash.jsonl"));
        let e = st.errors.iter().find(|e| e.message.contains("hash mismatch")).unwrap();
        let c = MISMATCH.captures(&e.message).unwrap();
        assert_eq!(&c[3], "sha256-RLkidqDUEmvxmhVv3JgudbiHRE3GL3eq0XkIPXEwl2M=");
        assert!(is_placeholder(&c[2]));
    }
}
