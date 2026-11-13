//! The shared context every engine builds on:
//! - what was being built (`Target`)
//! - the ownership map: store path -> user file / flake input
//! - the last good system generation (baseline for "what changed?")

use crate::util::{nix, run};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub enum Target {
    /// `nixosConfigurations.<host>` of a flake.
    Nixos { flake: String, host: String },
    /// `homeConfigurations.<user>` of a flake (standalone home-manager).
    Home { flake: String, user: String },
    /// Any flake installable, e.g. `.#foo` or `nixpkgs#hello`.
    Installable(String),
    /// Unknown (e.g. `nixplain explain` on a log file without --flake).
    Unknown,
}

impl Target {
    /// A flake installable; `…#nixosConfigurations.<host>[.config.system.build.toplevel]`
    /// and `…#homeConfigurations.<user>[.activationPackage]` become NixOS and
    /// home targets, so they get the same diagnosis as `nixplain os`/`home`.
    pub fn from_installable(i: &str) -> Target {
        let Some((flake, attr)) = i.split_once('#') else {
            return Target::Installable(i.to_string());
        };
        let flake = if flake.is_empty() { "." } else { flake };
        let segment = |s: &str| -> Option<(String, String)> {
            if let Some(q) = s.strip_prefix('"') {
                let (name, rest) = q.split_once('"')?;
                Some((name.to_string(), rest.to_string()))
            } else {
                let (name, rest) = s.split_at(s.find('.').unwrap_or(s.len()));
                (!name.is_empty()).then(|| (name.to_string(), rest.to_string()))
            }
        };
        if let Some(r) = attr.strip_prefix("nixosConfigurations.")
            && let Some((host, rest)) = segment(r)
            && (rest.is_empty() || rest == ".config.system.build.toplevel")
        {
            return Target::Nixos { flake: flake.to_string(), host };
        }
        if let Some(r) = attr.strip_prefix("homeConfigurations.")
            && let Some((user, rest)) = segment(r)
            && (rest.is_empty() || rest == ".activationPackage")
        {
            return Target::Home { flake: flake.to_string(), user };
        }
        Target::Installable(i.to_string())
    }

    pub fn toplevel_attr(&self) -> Option<String> {
        match self {
            Target::Nixos { flake, host } => Some(format!(
                "{flake}#nixosConfigurations.\"{host}\".config.system.build.toplevel"
            )),
            Target::Home { flake, user } => Some(format!("{flake}#homeConfigurations.\"{user}\".activationPackage")),
            Target::Installable(i) => Some(i.clone()),
            Target::Unknown => None,
        }
    }

    /// The flake reference part, if any.
    pub fn flake(&self) -> Option<&str> {
        match self {
            Target::Nixos { flake, .. } | Target::Home { flake, .. } => Some(flake),
            Target::Installable(i) => i.split_once('#').map(|(f, _)| f),
            Target::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum Owner {
    /// The user's own flake.
    User,
    /// A flake input, by its (possibly nested) name, e.g. `nixpkgs`.
    Input(String),
    /// `«string»`, `«nix-internal»` and similar.
    Internal,
    /// A store path we can't attribute.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Located {
    pub owner: Owner,
    /// Path shown to the user: relative path for user files,
    /// `<nixpkgs>/lib/attrsets.nix` for inputs.
    pub display: String,
    /// A readable file on disk (local checkout for user files, store path otherwise).
    pub path: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Ownership {
    /// (store root, owner). Self is always first.
    pub(crate) roots: Vec<(String, Owner)>,
    /// Where the user's flake lives on disk (the store copy of self maps here).
    pub local_root: Option<PathBuf>,
    /// The configuration (`nixosConfigurations.x` / `homeConfigurations.x`)
    /// whose module graph limits searches.
    pub(crate) scope_of: Option<(String, String)>,
    /// The user's files that are modules of that configuration, computed on
    /// first use. `None` inside: the graph could not be evaluated.
    pub(crate) scope: std::sync::OnceLock<Option<Vec<PathBuf>>>,
}

impl Ownership {
    /// Build the map for a local flake directory. Remote flakes and missing
    /// `nix` degrade to a map that only knows the local directory.
    pub fn for_flake(flake: Option<&str>) -> Ownership {
        let mut own = Ownership::default();
        let Some(flake) = flake else {
            own.local_root = std::env::current_dir().ok();
            return own;
        };
        let local = local_flake_dir(flake);
        own.local_root = local.as_ref().map(|d| git_root(d).unwrap_or_else(|| d.clone()));

        if let Ok(out) = run(nix().args(["flake", "archive", "--json", "--dry-run", flake]))
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&out)
        {
            if let Some(p) = v["path"].as_str() {
                own.roots.push((
                    p.to_string(),
                    if local.is_some() {
                        Owner::User
                    } else {
                        Owner::Input("self".into())
                    },
                ));
            }
            collect_inputs(&v["inputs"], "", &mut own.roots);
        }
        own
    }

    pub fn locate(&self, file: &str) -> Located {
        // Nix 2.3x prints files of flake inputs as `«path:/nix/store/…-source?…»/lib/x.nix`
        // or `«github:NixOS/nixpkgs/<rev>»/lib/x.nix`.
        if let Some(inner) = file.strip_prefix('«')
            && let Some((src, rest)) = inner.split_once("»/")
        {
            let base = src.split('?').next().unwrap_or(src);
            if let Some(store) = base.strip_prefix("path:").filter(|p| p.starts_with("/nix/store/")) {
                return self.locate(&format!("{store}/{rest}"));
            }
            let name = self
                .roots
                .iter()
                .find_map(|(_, o)| match o {
                    Owner::Input(n) if base.contains(n.rsplit('/').next().unwrap_or(n)) => Some(n.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    let in_nixpkgs = ["lib/", "pkgs/", "nixos/"].iter().any(|p| rest.starts_with(p));
                    if in_nixpkgs { "nixpkgs".into() } else { base.to_string() }
                });
            return Located {
                owner: Owner::Input(name.clone()),
                display: format!("<{name}>/{rest}"),
                path: None,
            };
        }
        if file.starts_with('«') {
            return Located {
                owner: Owner::Internal,
                display: file.to_string(),
                path: None,
            };
        }
        for (root, owner) in &self.roots {
            if let Some(rest) = file.strip_prefix(root.as_str()) {
                let rest = rest.trim_start_matches('/');
                return match owner {
                    Owner::User => {
                        let path = self.local_root.as_ref().map(|r| r.join(rest));
                        let display = path
                            .as_deref()
                            .map(crate::render::display_path)
                            .unwrap_or_else(|| rest.to_string());
                        Located {
                            owner: Owner::User,
                            display,
                            path,
                        }
                    }
                    Owner::Input(name) => Located {
                        owner: owner.clone(),
                        display: format!("<{name}>/{rest}"),
                        path: Some(PathBuf::from(file)),
                    },
                    _ => Located {
                        owner: owner.clone(),
                        display: file.into(),
                        path: Some(file.into()),
                    },
                };
            }
        }
        if !file.starts_with("/nix/store/") {
            // A plain path on disk: the user's if it is under their tree.
            let p = PathBuf::from(file);
            let mine = self.local_root.as_ref().is_some_and(|r| p.starts_with(r))
                || std::env::current_dir().is_ok_and(|c| p.starts_with(c));
            return Located {
                owner: if mine { Owner::User } else { Owner::Unknown },
                display: crate::render::display_path(&p),
                path: Some(p),
            };
        }
        // Unmapped store path: recognise nixpkgs by its layout.
        let rest = file.splitn(5, '/').nth(4).unwrap_or("");
        let in_nixpkgs = ["lib/", "pkgs/", "nixos/"].iter().any(|p| rest.starts_with(p));
        if in_nixpkgs {
            Located {
                owner: Owner::Input("nixpkgs".into()),
                display: format!("<nixpkgs>/{rest}"),
                path: Some(file.into()),
            }
        } else {
            Located {
                owner: Owner::Unknown,
                display: file.into(),
                path: Some(file.into()),
            }
        }
    }

    pub fn is_user(&self, file: &str) -> bool {
        self.locate(file).owner == Owner::User
    }

    /// Limit module searches to the modules of this NixOS configuration.
    pub fn scope_to(&mut self, target: &Target) {
        let (flake, attr) = match target {
            Target::Nixos { flake, host } => (flake, format!("nixosConfigurations.{}", crate::util::nix_str(host))),
            Target::Home { flake, user } => (flake, format!("homeConfigurations.{}", crate::util::nix_str(user))),
            _ => return,
        };
        let flake = local_flake_dir(flake)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| flake.clone());
        self.scope_of = Some((flake, attr));
    }

    /// The user's files that are modules of the evaluated configuration
    /// (from `nixosConfigurations.<host>.graph`). Searches for options,
    /// services and overrides use these, so a flake with several hosts does
    /// not point at another host's file. Falls back to every `.nix` file
    /// when the graph is unavailable (old nixpkgs, broken imports, no
    /// NixOS target).
    pub fn module_files(&self) -> Vec<PathBuf> {
        let scoped = self.scope.get_or_init(|| {
            let (flake, attr) = self.scope_of.as_ref()?;
            let expr = format!(
                "let f = builtins.getFlake {}; walk = n: [ (toString n.key) (toString n.file) ] ++ builtins.concatMap walk n.imports; \
                 in builtins.concatMap walk f.{attr}.graph",
                crate::util::nix_str(flake),
            );
            let out = run(nix().args(["eval", "--json", "--impure", "--expr", &expr])).ok()?;
            let keys: Vec<String> = serde_json::from_str(&out).ok()?;
            let mut files: Vec<PathBuf> = keys
                .iter()
                .filter(|k| k.ends_with(".nix"))
                .map(|k| self.locate(k))
                .filter(|l| l.owner == Owner::User)
                .filter_map(|l| l.path)
                .filter(|p| p.exists())
                .collect();
            files.sort();
            files.dedup();
            if std::env::var_os("NIXPLAIN_DEBUG").is_some() {
                eprintln!("module scope: {files:?}");
            }
            Some(files)
        });
        match scoped {
            Some(files) => files.clone(),
            None => self.user_nix_files(),
        }
    }

    /// All `.nix` files of the user's flake (for searching hashes, pnames, ...).
    pub fn user_nix_files(&self) -> Vec<PathBuf> {
        let mut out = vec![];
        if let Some(root) = &self.local_root {
            walk(root, &mut out, 0);
        }
        out
    }
}

fn collect_inputs(v: &serde_json::Value, prefix: &str, roots: &mut Vec<(String, Owner)>) {
    let Some(obj) = v.as_object() else { return };
    for (name, input) in obj {
        let full = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if let Some(p) = input["path"].as_str()
            && !roots.iter().any(|(r, _)| r == p)
        {
            roots.push((p.to_string(), Owner::Input(full.clone())));
        }
        collect_inputs(&input["inputs"], &full, roots);
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 || out.len() > 5000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "result" || name == "target" || name == "node_modules" {
            continue;
        }
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out, depth + 1),
            Ok(t) if t.is_file() && name.ends_with(".nix") => out.push(p),
            _ => {}
        }
    }
}

/// `.`, `/etc/nixos`, `path:/x`, `git+file:///x?dir=y` -> local directory.
pub fn local_flake_dir(flake: &str) -> Option<PathBuf> {
    let f = flake
        .strip_prefix("path:")
        .or_else(|| flake.strip_prefix("git+file://"))
        .or_else(|| flake.strip_prefix("file://"))
        .unwrap_or(flake);
    let (path, query) = f.split_once('?').unwrap_or((f, ""));
    if !(path.starts_with('/') || path.starts_with('.') || path.starts_with('~')) {
        return None;
    }
    let path = if let Some(rest) = path.strip_prefix("~/") {
        PathBuf::from(std::env::var("HOME").ok()?).join(rest)
    } else {
        PathBuf::from(path)
    };
    let mut dir = std::fs::canonicalize(&path).ok()?;
    if let Some(sub) = query.split('&').find_map(|kv| kv.strip_prefix("dir=")) {
        dir = dir.join(sub);
    }
    dir.join("flake.nix").exists().then_some(dir)
}

fn git_root(dir: &Path) -> Option<PathBuf> {
    let out = run(std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"]))
    .ok()?;
    let p = PathBuf::from(out.trim());
    // The flake's store copy is rooted at the git root only for git flakes;
    // for a flake in a subdirectory of a repo Nix copies the whole repo.
    p.exists().then_some(p)
}

/// The derivation of the last system that built successfully.
pub fn last_good_system_drv() -> Option<(String, String)> {
    let link = std::fs::read_link("/nix/var/nix/profiles/system").ok()?;
    let gen_name = link.file_name()?.to_string_lossy().to_string(); // system-1205-link
    let generation = gen_name
        .trim_start_matches("system-")
        .trim_end_matches("-link")
        .to_string();
    let out = std::fs::canonicalize("/nix/var/nix/profiles/system").ok()?;
    let drv = run(std::process::Command::new("nix-store").arg("-qd").arg(&out)).ok()?;
    let drv = drv.trim().to_string();
    (drv.ends_with(".drv") && Path::new(&drv).exists()).then_some((generation, drv))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmapped_nixpkgs_paths() {
        let own = Ownership::default();
        let l = own.locate("/nix/store/0ya4nbmac3fw13w4hmqyc41svdf7z352-source/lib/attrsets.nix");
        assert_eq!(l.owner, Owner::Input("nixpkgs".into()));
        assert_eq!(l.display, "<nixpkgs>/lib/attrsets.nix");
        assert_eq!(own.locate("«string»").owner, Owner::Internal);
    }

    #[test]
    fn virtual_input_paths() {
        let own = Ownership::default();
        let l = own.locate(
            "«path:/nix/store/0ya4nbmac3fw13w4hmqyc41svdf7z352-source?lastModified=0&narHash=x»/lib/customisation.nix",
        );
        assert_eq!(l.display, "<nixpkgs>/lib/customisation.nix");
        assert!(l.path.is_some());
        let l = own.locate("«github:NixOS/nixpkgs/abc»/lib/attrsets.nix");
        assert_eq!(l.display, "<nixpkgs>/lib/attrsets.nix");
    }

    #[test]
    fn mapped_roots() {
        let own = Ownership {
            roots: vec![
                ("/nix/store/aaaa-source".into(), Owner::User),
                ("/nix/store/bbbb-source".into(), Owner::Input("home-manager".into())),
            ],
            local_root: Some("/home/u/cfg".into()),
            ..Default::default()
        };
        let l = own.locate("/nix/store/aaaa-source/hosts/desktop.nix");
        assert_eq!(l.owner, Owner::User);
        assert_eq!(l.path.unwrap(), PathBuf::from("/home/u/cfg/hosts/desktop.nix"));
        assert_eq!(
            own.locate("/nix/store/bbbb-source/modules/x.nix").display,
            "<home-manager>/modules/x.nix"
        );
    }
}

#[cfg(test)]
mod target_tests {
    use super::Target;

    #[test]
    fn installable_to_nixos() {
        let t = Target::from_installable("/home/u/.dotfiles#nixosConfigurations.windoof.config.system.build.toplevel");
        assert!(matches!(t, Target::Nixos { ref flake, ref host } if flake == "/home/u/.dotfiles" && host == "windoof"));
        let t = Target::from_installable(".#nixosConfigurations.\"my.host\"");
        assert!(matches!(t, Target::Nixos { ref host, .. } if host == "my.host"));
        let t = Target::from_installable("#homeConfigurations.alice.activationPackage");
        assert!(matches!(t, Target::Home { ref flake, ref user } if flake == "." && user == "alice"));
        assert!(matches!(Target::from_installable(".#nixosConfigurations.x.config.foo"), Target::Installable(_)));
        assert!(matches!(Target::from_installable("nixpkgs#hello"), Target::Installable(_)));
    }
}
