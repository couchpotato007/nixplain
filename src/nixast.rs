//! Source lookups on the Nix syntax tree (rnix) instead of regexes.
//!
//! The central idea is `defs()`: every `a.b.c = value;` in a file together
//! with its *full* attribute path, including the paths of the attribute
//! sets it is nested in. Function bodies, `let … in`, `with`, `mkIf`/`mkMerge`
//! applications, lists and `//` keep the surrounding path, so
//!
//! ```nix
//! { config, ... }: {
//!   services.pipewire = lib.mkIf true {
//!     pulse.enabled = true;
//!   };
//! }
//! ```
//!
//! yields `services.pipewire` and `services.pipewire.pulse.enabled`. A
//! leading `config.` (modules with `options`/`config`) is ignored when
//! matching option paths.

use rnix::{SyntaxKind, SyntaxNode, TextRange, TextSize};
use std::path::Path;

/// A source span in editor terms: 1-based line and column, length in chars
/// (clamped to the first line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub column: usize,
    pub len: usize,
}

#[derive(Debug, Clone)]
pub struct Seg {
    /// Unquoted name (`"/data"` → `/data`), `${…}` for dynamic attributes.
    pub name: String,
    pub range: TextRange,
}

#[derive(Debug, Clone)]
pub struct Def {
    /// Full path, outer segments first.
    pub path: Vec<Seg>,
    /// How many of the trailing segments belong to this binding's own
    /// attrpath (the rest come from enclosing sets).
    pub own: usize,
    /// A `let` binding (not an option definition).
    pub is_let: bool,
    pub value: Option<SyntaxNode>,
}

pub struct File {
    pub src: String,
    pub root: SyntaxNode,
    line_starts: Vec<usize>,
}

impl File {
    pub fn parse(src: &str) -> File {
        let root = rnix::Root::parse(src).syntax();
        let mut line_starts = vec![0];
        line_starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
        File {
            src: src.to_string(),
            root,
            line_starts,
        }
    }

    pub fn load(path: &Path) -> Option<File> {
        Some(File::parse(&std::fs::read_to_string(path).ok()?))
    }

    /// Byte offset of a 1-based line/column (column counted in chars).
    pub fn offset(&self, line: usize, column: usize) -> Option<usize> {
        let start = *self.line_starts.get(line.checked_sub(1)?)?;
        let rest = &self.src[start..];
        let byte = rest
            .char_indices()
            .nth(column.saturating_sub(1))
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        Some(start + byte)
    }

    pub fn span(&self, range: TextRange) -> Span {
        let start: usize = range.start().into();
        let end: usize = range.end().into();
        let line_idx = self.line_starts.partition_point(|&s| s <= start) - 1;
        let line_start = self.line_starts[line_idx];
        let line_end = self.src[start..]
            .find('\n')
            .map(|i| start + i)
            .unwrap_or(self.src.len());
        let column = self.src[line_start..start].chars().count() + 1;
        let len = self.src[start..end.min(line_end).max(start)].chars().count().max(1);
        Span {
            line: line_idx + 1,
            column,
            len,
        }
    }

    /// All bindings with their full attribute paths.
    pub fn defs(&self) -> Vec<Def> {
        let mut out = vec![];
        walk(&self.root, &[], &mut out);
        out
    }

    /// Smallest node of `kind` that starts at `offset`.
    pub fn node_starting_at(&self, offset: usize, kind: SyntaxKind) -> Option<SyntaxNode> {
        let off = TextSize::from(offset as u32);
        self.root
            .descendants()
            .filter(|n| n.kind() == kind && n.text_range().start() == off)
            .min_by_key(|n| n.text_range().len())
    }
}

fn seg_name(attr: &SyntaxNode) -> String {
    match attr.kind() {
        SyntaxKind::NODE_STRING => string_content(attr).unwrap_or_else(|| attr.text().to_string()),
        SyntaxKind::NODE_DYNAMIC => "${…}".into(),
        _ => attr.text().to_string(),
    }
}

/// Content of a string literal without interpolations, `None` otherwise.
pub fn string_content(node: &SyntaxNode) -> Option<String> {
    if node.kind() != SyntaxKind::NODE_STRING {
        return None;
    }
    let mut s = String::new();
    for el in node.children_with_tokens() {
        match el {
            rnix::NodeOrToken::Token(t) if t.kind() == SyntaxKind::TOKEN_STRING_CONTENT => s.push_str(t.text()),
            rnix::NodeOrToken::Token(_) => {}
            rnix::NodeOrToken::Node(_) => return None, // interpolation
        }
    }
    Some(s)
}

fn walk(node: &SyntaxNode, prefix: &[Seg], out: &mut Vec<Def>) {
    for child in node.children() {
        match child.kind() {
            SyntaxKind::NODE_ATTRPATH_VALUE => {
                let is_let = node.kind() == SyntaxKind::NODE_LET_IN || node.kind() == SyntaxKind::NODE_LEGACY_LET;
                let Some(attrpath) = child.children().find(|c| c.kind() == SyntaxKind::NODE_ATTRPATH) else {
                    continue;
                };
                let own: Vec<Seg> = attrpath
                    .children()
                    .map(|a| Seg {
                        name: seg_name(&a),
                        range: a.text_range(),
                    })
                    .collect();
                let base: &[Seg] = if is_let { &[] } else { prefix };
                let mut path = base.to_vec();
                path.extend(own.iter().cloned());
                let value = child.children().find(|c| c.kind() != SyntaxKind::NODE_ATTRPATH);
                out.push(Def {
                    path: path.clone(),
                    own: own.len(),
                    is_let,
                    value: value.clone(),
                });
                if let Some(v) = value {
                    // a let binding's value starts a fresh path
                    walk_value(&v, if is_let { &own } else { &path }, out);
                }
            }
            SyntaxKind::NODE_ATTRPATH => {}
            _ => walk(&child, prefix, out),
        }
    }
}

/// Walk an expression that is the value of `prefix`.
fn walk_value(node: &SyntaxNode, prefix: &[Seg], out: &mut Vec<Def>) {
    match node.kind() {
        SyntaxKind::NODE_ATTR_SET => walk(node, prefix, out),
        _ => {
            // through lambdas, applications (mkIf, mkMerge, overrideAttrs), lists,
            // parens, let, with, if, `//`
            for c in node.children() {
                walk_value(&c, prefix, out);
            }
            if node.kind() == SyntaxKind::NODE_LET_IN {
                walk(node, prefix, out);
            }
        }
    }
}

impl Def {
    /// Path as option names, without a leading `config`.
    pub fn option_path(&self) -> Vec<&str> {
        let names: Vec<&str> = self.path.iter().map(|s| s.name.as_str()).collect();
        if names.first() == Some(&"config") && names.len() > 1 {
            names[1..].to_vec()
        } else {
            names
        }
    }

    /// Range covering the own segments `from..to` of the full path.
    pub fn segs_range(&self, from: usize, to: usize) -> Option<TextRange> {
        let first_own = self.path.len() - self.own;
        let from = from.max(first_own);
        if from >= to || to > self.path.len() {
            return None;
        }
        Some(TextRange::new(
            self.path[from].range.start(),
            self.path[to - 1].range.end(),
        ))
    }

    pub fn last_seg(&self) -> &str {
        self.path.last().map(|s| s.name.as_str()).unwrap_or("")
    }
}

/// Split an option path from a Nix message: `fileSystems."/a.b".fsType`.
pub fn split_option(opt: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut quoted = false;
    for ch in opt.chars() {
        match ch {
            '"' => quoted = !quoted,
            '.' if !quoted => parts.push(String::new()),
            c => parts.last_mut().unwrap().push(c),
        }
    }
    parts
}

/// Inverse of `split_option`: quote segments that are not plain identifiers.
pub fn join_option(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| {
            let plain = p.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && p.chars().all(|c| c.is_ascii_alphanumeric() || "_-'".contains(c));
            if plain { p.clone() } else { format!("\"{p}\"") }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Where `opt` is defined in `file`: exact definitions first (span on the
/// last segment), otherwise definitions below it (span on the segments of
/// `opt` in the binding's own attrpath). Also matches suffixes of `opt` of at
/// least `min_suffix` segments, for files that are imported at a nested
/// level (e.g. a home-manager module defining `home.packages`).
pub fn find_option(file: &File, opt: &str, min_suffix: usize) -> Vec<Span> {
    let want = split_option(opt);
    let defs: Vec<Def> = file.defs().into_iter().filter(|d| !d.is_let).collect();
    for skip in 0..want.len() {
        let w = &want[skip..];
        if skip > 0 && w.len() < min_suffix {
            break;
        }
        // exact
        let exact: Vec<Span> = defs
            .iter()
            .filter(|d| d.option_path() == w.iter().map(String::as_str).collect::<Vec<_>>())
            .map(|d| file.span(d.path.last().unwrap().range))
            .collect();
        if !exact.is_empty() {
            return exact;
        }
        // deeper definitions: `opt.something = …`
        let mut below: Vec<Span> = vec![];
        for d in &defs {
            let p = d.option_path();
            let offset = d.path.len() - p.len(); // 1 when `config.` was stripped
            if p.len() > w.len() && p[..w.len()] == w.iter().map(String::as_str).collect::<Vec<_>>()[..] {
                // the deepest *own* segment inside `opt`, so a set nested in
                // `opt = { … }` points at its own key, not the outer one
                if let Some(r) = d.segs_range(0, offset + w.len()) {
                    let s = file.span(r);
                    if !below.contains(&s) {
                        below.push(s);
                    }
                }
            }
        }
        if !below.is_empty() {
            below.sort_by_key(|s| (s.line, s.column));
            return below;
        }
    }
    vec![]
}

/// Elements of the list(s) in the definitions of `opt` (through `with`,
/// parens and `++`), as (name, span): `pkgs.nheko` → `nheko`.
pub fn list_items(file: &File, opt: &str, min_suffix: usize) -> Vec<(String, Span, SyntaxNode)> {
    let want = split_option(opt);
    let defs: Vec<Def> = file.defs().into_iter().filter(|d| !d.is_let).collect();
    for skip in 0..want.len() {
        let w: Vec<&str> = want[skip..].iter().map(String::as_str).collect();
        if skip > 0 && w.len() < min_suffix {
            break;
        }
        let mut out = vec![];
        for d in defs.iter().filter(|d| d.option_path() == w) {
            if let Some(v) = &d.value {
                collect_items(file, v, &mut out);
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    vec![]
}

fn collect_items(file: &File, node: &SyntaxNode, out: &mut Vec<(String, Span, SyntaxNode)>) {
    match node.kind() {
        SyntaxKind::NODE_LIST => {
            for item in node.children() {
                let name = item_name(&item);
                // underline the attribute name, not `pkgs.`
                let range = match item.kind() {
                    SyntaxKind::NODE_SELECT => item.children().last().map(|a| a.text_range()),
                    _ => None,
                }
                .unwrap_or(item.text_range());
                out.push((name, file.span(range), item));
            }
        }
        SyntaxKind::NODE_WITH => {
            if let Some(body) = node.children().last() {
                collect_items(file, &body, out);
            }
        }
        SyntaxKind::NODE_PAREN | SyntaxKind::NODE_BIN_OP | SyntaxKind::NODE_APPLY => {
            for c in node.children() {
                collect_items(file, &c, out);
            }
        }
        _ => {}
    }
}

/// `pkgs.foo` → `foo`, `foo` → `foo`, `(pkgs.callPackage ./x.nix {})` → its text.
fn item_name(item: &SyntaxNode) -> String {
    match item.kind() {
        SyntaxKind::NODE_IDENT => item.text().to_string(),
        SyntaxKind::NODE_SELECT => item
            .children()
            .last()
            .and_then(|attrpath| attrpath.children().last())
            .map(|a| seg_name(&a))
            .unwrap_or_else(|| item.text().to_string()),
        _ => item.text().to_string(),
    }
}

/// The string literals in `file` whose content is exactly `content`.
pub fn find_strings(file: &File, content: &str) -> Vec<(Span, SyntaxNode)> {
    file.root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NODE_STRING)
        .filter(|n| string_content(n).as_deref() == Some(content))
        .map(|n| {
            // the content, without the quotes
            let r = n.text_range();
            let inner = TextRange::new(r.start() + TextSize::from(1), r.end() - TextSize::from(1));
            (file.span(inner), n)
        })
        .collect()
}

/// The definition a node is the value of (walking up through parens etc.).
pub fn enclosing_binding(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.ancestors().find(|a| a.kind() == SyntaxKind::NODE_ATTRPATH_VALUE)
}

/// Key of an `a.b = value;` node.
pub fn binding_key(binding: &SyntaxNode) -> Option<String> {
    let attrpath = binding.children().find(|c| c.kind() == SyntaxKind::NODE_ATTRPATH)?;
    attrpath.children().last().map(|a| seg_name(&a))
}

/// Sibling bindings of `binding` in the same attribute set (and the sets
/// around it), nearest first: used to find `version = "…"` next to a hash.
pub fn nearby_binding(file: &File, binding: &SyntaxNode, key: &str) -> Option<(Span, String)> {
    for set in binding.ancestors().filter(|a| a.kind() == SyntaxKind::NODE_ATTR_SET) {
        for b in set.children().filter(|c| c.kind() == SyntaxKind::NODE_ATTRPATH_VALUE) {
            if binding_key(&b).as_deref() == Some(key)
                && let Some(v) = b.children().find(|c| c.kind() == SyntaxKind::NODE_STRING)
            {
                return Some((file.span(v.text_range()), string_content(&v).unwrap_or_default()));
            }
        }
    }
    None
}

/// For a call `f { a = 1; b = 2; }` starting at `offset`: the span of the
/// key `name` in the argument set, if passed.
pub fn call_argument(file: &File, offset: usize, name: &str) -> Option<Span> {
    let apply = file
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NODE_APPLY && usize::from(n.text_range().start()) == offset)
        .max_by_key(|n| n.text_range().len())?;
    let arg = apply.children().last()?;
    let set = arg.descendants().find(|n| n.kind() == SyntaxKind::NODE_ATTR_SET)?;
    set.children()
        .filter(|c| c.kind() == SyntaxKind::NODE_ATTRPATH_VALUE)
        .find(|b| binding_key(b).as_deref() == Some(name))
        .and_then(|b| b.children().find(|c| c.kind() == SyntaxKind::NODE_ATTRPATH))
        .map(|a| file.span(a.text_range()))
}

/// The whole call starting at `offset` (first line only).
pub fn call_span(file: &File, offset: usize) -> Option<Span> {
    let apply = file
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NODE_APPLY && usize::from(n.text_range().start()) == offset)
        .max_by_key(|n| n.text_range().len())?;
    Some(file.span(apply.text_range()))
}

/// The argument pattern `{ a, b }` of the function starting at `offset`.
pub fn lambda_pattern(file: &File, offset: usize) -> Option<(Span, String)> {
    let lambda = file.node_starting_at(offset, SyntaxKind::NODE_LAMBDA)?;
    let pat = lambda.children().find(|c| c.kind() == SyntaxKind::NODE_PATTERN)?;
    let text: String = pat.text().to_string().split_whitespace().collect::<Vec<_>>().join(" ");
    Some((file.span(pat.text_range()), text))
}

/// The entry `name` in the argument pattern of the file's top-level function
/// (`{ stdenv, libfoo }:` → `libfoo`).
pub fn pattern_entry(file: &File, name: &str) -> Option<Span> {
    file.root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NODE_PAT_ENTRY)
        .find(|e| e.children().next().map(|i| i.text().to_string()).as_deref() == Some(name))
        .map(|e| file.span(e.children().next().unwrap().text_range()))
}

/// Parts of an application `f a b`: the function's text and the arguments.
pub fn apply_parts(node: &SyntaxNode) -> Option<(String, Vec<SyntaxNode>)> {
    let mut node = node.clone();
    while node.kind() == SyntaxKind::NODE_PAREN {
        node = node.children().next()?;
    }
    if node.kind() != SyntaxKind::NODE_APPLY {
        return None;
    }
    let mut args = vec![];
    let mut cur = node;
    while cur.kind() == SyntaxKind::NODE_APPLY {
        let mut ch = cur.children();
        let f = ch.next()?;
        args.push(ch.next()?);
        cur = f;
    }
    args.reverse();
    Some((cur.text().to_string(), args))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODULE: &str = r#"{ config, lib, pkgs, ... }:
let
  helper = { a, b }: a;
in
{
  options.my.gui = lib.mkEnableOption "gui";

  config = lib.mkIf true {
    services.pipewire = {
      enable = true;
      pulse.enabled = true;
    };
    fileSystems."/data".device = "/dev/sdb1";
    environment.systemPackages = with pkgs; [ hello pkgs.nheko (callPackage ./x.nix { }) ];
    users.users.alice.packages = [ pkgs.quaternion ] ++ [ pkgs.jq ];
  };
}
"#;

    #[test]
    fn nested_paths() {
        let f = File::parse(MODULE);
        let s = find_option(&f, "services.pipewire.pulse.enabled", 99);
        assert_eq!(
            s,
            vec![Span {
                line: 11,
                column: 13,
                len: 7
            }]
        );
        // a definition below the option (no exact one)
        let s = find_option(&f, "fileSystems.\"/data\"", 99);
        assert_eq!(
            s,
            vec![Span {
                line: 13,
                column: 5,
                len: 19
            }]
        );
        // unknown option prefix
        let s = find_option(&f, "services.pipewire", 99);
        assert_eq!(s[0].line, 9);
    }

    #[test]
    fn list_items_through_with_and_concat() {
        let f = File::parse(MODULE);
        let items: Vec<String> = list_items(&f, "environment.systemPackages", 99)
            .into_iter()
            .map(|i| i.0)
            .collect();
        assert_eq!(items[..2], ["hello", "nheko"]);
        let items = list_items(&f, "users.users.alice.packages", 99);
        assert_eq!(
            items.iter().map(|i| i.0.as_str()).collect::<Vec<_>>(),
            ["quaternion", "jq"]
        );
        assert_eq!(
            items[0].1,
            Span {
                line: 15,
                column: 41,
                len: 10
            }
        );
    }

    #[test]
    fn suffix_match_for_nested_modules() {
        let f = File::parse("{ pkgs, ... }: { home.packages = [ pkgs.nheko ]; }");
        let items = list_items(&f, "home-manager.users.alice.home.packages", 2);
        assert_eq!(items[0].0, "nheko");
    }

    #[test]
    fn strings_and_neighbours() {
        let f = File::parse(
            "{ fetchurl }: rec {\n  version = \"1.2\";\n  src = fetchurl {\n    hash = \"sha256-X\";\n  };\n}\n",
        );
        let (span, node) = find_strings(&f, "sha256-X").pop().unwrap();
        assert_eq!(
            span,
            Span {
                line: 4,
                column: 13,
                len: 8
            }
        );
        let b = enclosing_binding(&node).unwrap();
        assert_eq!(binding_key(&b).as_deref(), Some("hash"));
        assert_eq!(nearby_binding(&f, &b, "version").unwrap().1, "1.2");
    }

    #[test]
    fn calls_and_patterns() {
        let src = "let\n  mkUser = { name, uid }: { };\nin\n{\n  users.users = mkUser {\n    name = \"bob\";\n    shell = \"/bin/zsh\";\n  };\n}\n";
        let f = File::parse(src);
        let call = f.offset(5, 17).unwrap();
        assert_eq!(
            call_argument(&f, call, "shell"),
            Some(Span {
                line: 7,
                column: 5,
                len: 5
            })
        );
        let def = f.offset(2, 12).unwrap();
        assert_eq!(lambda_pattern(&f, def).unwrap().1, "{ name, uid }");
        let f = File::parse("{ stdenv, libfoo }:\nstdenv.mkDerivation { }\n");
        assert_eq!(
            pattern_entry(&f, "libfoo"),
            Some(Span {
                line: 1,
                column: 11,
                len: 6
            })
        );
    }
}
