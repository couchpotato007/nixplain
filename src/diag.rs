//! The diagnostic data model. Engines produce `Diagnostic`s, renderers
//! turn them into text (human / short) or JSON.

use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    /// Stable error code, e.g. `meta/insecure`. Used by `--explain`.
    pub code: String,
    pub title: String,
    /// Primary location in the user's code, if known (for editors / LSP).
    pub location: Option<Location>,
    /// Content in display order.
    pub blocks: Vec<Block>,
    /// Machine-applicable edits (`nixplain --fix`).
    pub edits: Vec<Edit>,
    /// One-line summary for `--short`.
    pub short: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    /// `  key: value`
    Note { key: String, value: String },
    /// A titled block of pre-formatted lines (trees, lists, log excerpts).
    Section { title: String, lines: Vec<String> },
    /// An annotated source excerpt.
    Snippet(Snippet),
    /// Numbered fixes, each optionally with Nix code.
    Fixes { fixes: Vec<Fix> },
}

#[derive(Debug, Clone, Serialize)]
pub struct Snippet {
    pub title: Option<String>,
    /// Path as shown to the user.
    pub display: String,
    /// Path to read the source from.
    pub path: PathBuf,
    pub labels: Vec<Label>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Label {
    pub line: usize,
    /// 1-based column.
    pub column: usize,
    /// Length of the underline in characters (at least 1).
    pub len: usize,
    pub text: String,
    pub primary: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fix {
    pub title: String,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Edit {
    pub file: PathBuf,
    pub line: usize,
    pub old: String,
    pub new: String,
}

impl Diagnostic {
    pub fn new(code: &str, title: impl Into<String>) -> Self {
        let title = title.into();
        Diagnostic {
            code: code.into(),
            short: title.clone(),
            title,
            location: None,
            blocks: vec![],
            edits: vec![],
        }
    }

    pub fn note(&mut self, key: &str, value: impl Into<String>) -> &mut Self {
        self.blocks.push(Block::Note {
            key: key.into(),
            value: value.into(),
        });
        self
    }

    pub fn section(&mut self, title: impl Into<String>, lines: Vec<String>) -> &mut Self {
        if !lines.is_empty() {
            self.blocks.push(Block::Section {
                title: title.into(),
                lines,
            });
        }
        self
    }

    pub fn snippet(&mut self, s: Snippet) -> &mut Self {
        if self.location.is_none()
            && let Some(l) = s.labels.iter().find(|l| l.primary)
        {
            self.location = Some(Location {
                file: s.path.clone(),
                line: l.line,
                column: l.column,
            });
        }
        self.blocks.push(Block::Snippet(s));
        self
    }

    pub fn fixes(&mut self, fixes: Vec<Fix>) -> &mut Self {
        if !fixes.is_empty() {
            self.blocks.push(Block::Fixes { fixes });
        }
        self
    }
}

impl Fix {
    pub fn text(title: impl Into<String>) -> Self {
        Fix {
            title: title.into(),
            code: None,
        }
    }
    pub fn code(title: impl Into<String>, code: impl Into<String>) -> Self {
        Fix {
            title: title.into(),
            code: Some(code.into()),
        }
    }
}

impl Label {
    pub fn primary(line: usize, column: usize, len: usize, text: impl Into<String>) -> Self {
        Label {
            line,
            column,
            len: len.max(1),
            text: text.into(),
            primary: true,
        }
    }
    pub fn secondary(line: usize, column: usize, len: usize, text: impl Into<String>) -> Self {
        Label {
            line,
            column,
            len: len.max(1),
            text: text.into(),
            primary: false,
        }
    }
}
