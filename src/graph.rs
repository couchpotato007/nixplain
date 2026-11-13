//! The diagnostic second run: evaluate the target again with meta checks
//! relaxed (through `extendModules` for NixOS, environment variables
//! otherwise), then answer "how does X get into my system?" on the
//! derivation graph.

use crate::context::{Ownership, Target, local_flake_dir};
use crate::util::{drv_name, nix, nix_str, run};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::process::Command;

/// Environment variables that relax nixpkgs' meta checks. Used in addition
/// to the module, because `nixpkgs.config` is ignored when `nixpkgs.pkgs` is set.
const RELAX_ENV: &[(&str, &str)] = &[
    ("NIXPKGS_ALLOW_INSECURE", "1"),
    ("NIXPKGS_ALLOW_UNFREE", "1"),
    ("NIXPKGS_ALLOW_BROKEN", "1"),
    ("NIXPKGS_ALLOW_UNSUPPORTED_SYSTEM", "1"),
    ("NIXPKGS_ALLOW_NONSOURCE", "1"),
];

/// Which option (and file) put a package into the system.
#[derive(Debug, Clone, Deserialize)]
pub struct ProvDef {
    pub option: String,
    pub file: String,
    pub names: Vec<Option<String>>,
}

#[derive(Debug, Default)]
pub struct SystemEval {
    pub toplevel_drv: String,
    pub provenance: Vec<ProvDef>,
}

/// Nix expression for the instrumented NixOS evaluation. The user's files
/// are untouched; we only add a module through `extendModules`.
fn nixos_expr(flake: &str, host: &str) -> String {
    format!(
        r#"
let
  flake = builtins.getFlake {flake};
  base = flake.nixosConfigurations.{host};
  sys = base.extendModules {{
    modules = [ {{
      nixpkgs.config = {{
        allowInsecurePredicate = _: true;
        allowUnfree = true;
        allowBroken = true;
        allowUnsupportedSystem = true;
        allowNonSource = true;
      }};
    }} ];
  }};
  safeName = p: let r = builtins.tryEval (if builtins.isAttrs p then p.name or null else null);
                in if r.success then r.value else null;
  names = v: if builtins.isList v then map safeName v else [ ];
  fromOption = option: opt:
    map (d: {{ inherit option; file = d.file; names = names d.value; }}) opt.definitionsWithLocations;
  userPackages =
    let
      r = builtins.tryEval (builtins.concatMap (d:
        if builtins.isAttrs d.value then
          builtins.concatMap (u:
            let v = d.value.${{u}}; in
            if builtins.isAttrs v && v ? packages then
              [ {{ option = "users.users.${{u}}.packages"; file = d.file; names = names v.packages; }} ]
            else [ ])
          (builtins.attrNames d.value)
        else [ ]) sys.options.users.users.definitionsWithLocations);
    in if r.success then r.value else [ ];
  # home-manager as a NixOS module: every user's home.packages, through the
  # submodule evaluation that `valueMeta` exposes (nixpkgs 25.11+)
  hmUsers =
    let r = builtins.tryEval (sys.options.home-manager.users.valueMeta.attrs or {{ }});
    in if sys.options ? home-manager && r.success then r.value else {{ }};
  homePackages = builtins.concatMap (u:
    let c = hmUsers.${{u}}.configuration or null; in
    if c == null || !(c.options ? home) then [ ]
    else fromOption "home-manager.users.${{u}}.home.packages" c.options.home.packages)
    (builtins.attrNames hmUsers);
in {{
  toplevel = sys.config.system.build.toplevel.drvPath;
  provenance = fromOption "environment.systemPackages" sys.options.environment.systemPackages
    ++ userPackages ++ homePackages;
}}"#,
        flake = nix_str(flake),
        host = nix_str(host),
    )
}

/// Standalone home-manager: `homeConfigurations.<user>`.
fn home_expr(flake: &str, user: &str) -> String {
    format!(
        r#"
let
  flake = builtins.getFlake {flake};
  hc = flake.homeConfigurations.{user};
  safeName = p: let r = builtins.tryEval (if builtins.isAttrs p then p.name or null else null);
                in if r.success then r.value else null;
  names = v: if builtins.isList v then map safeName v else [ ];
in {{
  toplevel = hc.activationPackage.drvPath;
  provenance = map (d: {{ option = "home.packages"; file = d.file; names = names d.value; }})
    hc.options.home.packages.definitionsWithLocations;
}}"#,
        flake = nix_str(flake),
        user = nix_str(user),
    )
}

pub fn evaluate(target: &Target) -> Result<SystemEval> {
    match target {
        Target::Home { flake, user } => {
            let flake_ref = local_flake_dir(flake)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| flake.clone());
            let mut cmd = nix();
            cmd.args(["eval", "--json", "--impure", "--expr", &home_expr(&flake_ref, user)]);
            // the user's pkgs are usually passed in, so only the environment
            // variables can relax the checks
            cmd.envs(RELAX_ENV.iter().copied());
            let out = run(&mut cmd).context("diagnostic evaluation of the home configuration failed")?;
            #[derive(Deserialize)]
            struct Out {
                toplevel: String,
                provenance: Vec<ProvDef>,
            }
            let o: Out = serde_json::from_str(&out)?;
            Ok(SystemEval {
                toplevel_drv: o.toplevel,
                provenance: o.provenance,
            })
        }
        Target::Nixos { flake, host } => {
            let flake_ref = local_flake_dir(flake)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| flake.clone());
            let mut cmd = nix();
            cmd.args(["eval", "--json", "--impure", "--expr", &nixos_expr(&flake_ref, host)]);
            cmd.envs(RELAX_ENV.iter().copied());
            let out = run(&mut cmd).context("diagnostic evaluation of the NixOS configuration failed")?;
            #[derive(Deserialize)]
            struct Out {
                toplevel: String,
                provenance: Vec<ProvDef>,
            }
            let o: Out = serde_json::from_str(&out)?;
            Ok(SystemEval {
                toplevel_drv: o.toplevel,
                provenance: o.provenance,
            })
        }
        Target::Installable(i) => {
            let attr = if i.contains('#') {
                format!("{i}.drvPath")
            } else {
                format!("{i}#default.drvPath")
            };
            let mut cmd = nix();
            cmd.args(["eval", "--raw", "--impure", &attr]);
            cmd.envs(RELAX_ENV.iter().copied());
            let out = run(&mut cmd).context("diagnostic evaluation failed")?;
            Ok(SystemEval {
                toplevel_drv: out.trim().to_string(),
                provenance: vec![],
            })
        }
        Target::Unknown => bail!("no target known (pass --flake/--host or --installable)"),
    }
}

/// The derivation graph of a closure. Nodes are drv basenames.
#[derive(Debug, Default)]
pub struct DrvGraph {
    pub root: String,
    /// node -> its direct dependencies
    pub deps: HashMap<String, Vec<String>>,
    /// node -> direct dependents
    pub rdeps: HashMap<String, Vec<String>>,
}

impl DrvGraph {
    pub fn load(root_drv: &str) -> Result<DrvGraph> {
        let dot = run(Command::new("nix-store").args(["--query", "--graph", root_drv]))?;
        Ok(Self::from_dot(root_drv, &dot))
    }

    /// `nix-store --query --graph` prints `"dep" -> "referrer"`.
    pub fn from_dot(root_drv: &str, dot: &str) -> DrvGraph {
        let mut g = DrvGraph {
            root: root_drv.rsplit('/').next().unwrap_or(root_drv).to_string(),
            ..Default::default()
        };
        for line in dot.lines() {
            let mut parts = line.split('"');
            let (Some(_), Some(dep), Some(arrow), Some(referrer)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if arrow.trim() != "->" {
                continue;
            }
            g.deps.entry(referrer.to_string()).or_default().push(dep.to_string());
            g.rdeps.entry(dep.to_string()).or_default().push(referrer.to_string());
        }
        for v in g.deps.values_mut() {
            v.sort();
            v.dedup();
        }
        g
    }

    pub fn find_by_name(&self, name: &str) -> Vec<String> {
        let mut v: Vec<String> = self.rdeps.keys().filter(|n| drv_name(n) == name).cloned().collect();
        v.sort();
        v
    }

    /// All nodes from which one of `targets` is reachable (targets included).
    pub fn reaching(&self, targets: &[String]) -> HashSet<String> {
        let mut seen: HashSet<String> = targets.iter().cloned().collect();
        let mut q: VecDeque<String> = targets.iter().cloned().collect();
        while let Some(n) = q.pop_front() {
            for r in self.rdeps.get(&n).into_iter().flatten() {
                if seen.insert(r.clone()) {
                    q.push_back(r.clone());
                }
            }
        }
        seen
    }

    /// Shortest dependency path from `from` to any of `targets`.
    pub fn path(&self, from: &str, targets: &HashSet<String>) -> Option<Vec<String>> {
        self.path_avoiding(from, targets, None)
    }

    /// Shortest path that does not pass through `avoid`.
    pub fn path_avoiding(&self, from: &str, targets: &HashSet<String>, avoid: Option<&str>) -> Option<Vec<String>> {
        let mut prev: HashMap<String, String> = HashMap::new();
        let mut q = VecDeque::from([from.to_string()]);
        let mut seen = HashSet::from([from.to_string()]);
        while let Some(n) = q.pop_front() {
            if targets.contains(&n) {
                let mut path = vec![n.clone()];
                let mut cur = n;
                while let Some(p) = prev.get(&cur) {
                    path.push(p.clone());
                    cur = p.clone();
                }
                path.reverse();
                return Some(path);
            }
            for d in self.deps.get(&n).into_iter().flatten() {
                if Some(d.as_str()) == avoid {
                    continue;
                }
                if seen.insert(d.clone()) {
                    prev.insert(d.clone(), n.clone());
                    q.push_back(d.clone());
                }
            }
        }
        None
    }
}

/// One way a target enters the system.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Package name at the entry point (e.g. `xsane-0.999`).
    pub name: String,
    /// Chain of drv names from the root to the target.
    pub chain: Vec<String>,
    /// Options/files that add this entry point, if known.
    pub added_by: Vec<(String, String)>,
}

/// Find every top-level way `targets` enter the closure of `g.root`:
/// each package in `system-path` (and each other child of the toplevel)
/// whose closure contains a target.
pub fn entries(g: &DrvGraph, targets: &[String], prov: &[ProvDef]) -> Vec<Entry> {
    let reach = g.reaching(targets);
    let tset: HashSet<String> = targets.iter().cloned().collect();
    let mut prov_by_name: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for d in prov {
        for n in d.names.iter().flatten() {
            prov_by_name
                .entry(n.clone())
                .or_default()
                .push((d.option.clone(), d.file.clone()));
        }
    }

    let mut out = vec![];
    let root_children: Vec<&String> = g
        .deps
        .get(&g.root)
        .into_iter()
        .flatten()
        .filter(|c| reach.contains(*c))
        .collect();
    // Everything reachable through system-path is reported per package;
    // other children (etc, activate, …) only count if they reach the
    // target some other way.
    let system_path = root_children.iter().find(|c| is_env(c)).map(|s| s.as_str());
    for c in root_children.iter().copied() {
        let is_sp = is_env(c);
        let starts: Vec<&String> = if is_sp {
            g.deps
                .get(c)
                .into_iter()
                .flatten()
                .filter(|x| reach.contains(*x))
                .collect()
        } else {
            vec![c]
        };
        for s in starts {
            let avoid = if is_sp { None } else { system_path };
            let Some(tail) = g.path_avoiding(s, &tset, avoid) else {
                continue;
            };
            let mut chain = vec![drv_name(&g.root)];
            if is_sp {
                chain.push(drv_name(c));
            }
            chain.extend(tail.iter().map(|n| drv_name(n)));
            // The entry point is the first node on the path that an option
            // put there (system-path children, per-user profiles, …).
            let named = tail.iter().map(|n| drv_name(n)).find(|n| prov_by_name.contains_key(n));
            let name = named.unwrap_or_else(|| drv_name(s));
            let added_by = prov_by_name.get(&name).cloned().unwrap_or_default();
            out.push(Entry { name, chain, added_by });
        }
    }
    if out.is_empty()
        && let Some(p) = g.path(&g.root, &tset)
    {
        let chain: Vec<String> = p.iter().map(|n| drv_name(n)).collect();
        out.push(Entry {
            name: chain.get(1).cloned().unwrap_or_default(),
            chain,
            added_by: vec![],
        });
    }
    out
}

/// The buildEnv that holds the user-listed packages: `system-path` for NixOS,
/// `home-manager-path` for home-manager.
fn is_env(node: &str) -> bool {
    matches!(drv_name(node).as_str(), "system-path" | "home-manager-path")
}

/// Sort entries: user-added first, then module-added, then the rest.
pub fn rank(entries: &mut [Entry], own: &Ownership) {
    entries.sort_by_key(|e| {
        let user = e.added_by.iter().any(|(_, f)| own.is_user(f));
        let known = !e.added_by.is_empty();
        (!user, !known, e.chain.len(), e.name.clone())
    });
}

/// Render a chain as an indented tree, collapsing the middle of long chains.
pub fn tree(chain: &[String]) -> Vec<String> {
    let mut items: Vec<String> = chain.to_vec();
    if items.len() > 8 {
        let hidden = items.len() - 6;
        let mut v = items[..3].to_vec();
        v.push(format!("… {hidden} more …"));
        v.extend_from_slice(&items[items.len() - 3..]);
        items = v;
    }
    items
        .iter()
        .enumerate()
        .map(|(i, n)| {
            if i == 0 {
                // `nixos-system-demo-26.11.19700101.dirty` → `nixos-system-demo`
                if n.starts_with("nixos-system-") {
                    crate::util::pname(n)
                } else {
                    n.clone()
                }
            } else {
                format!("{}└─ {n}", "   ".repeat(i - 1))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOT: &str = r#"digraph G {
"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-gtk2-2.24.33.drv" -> "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-xsane-0.999.drv" [color = "black"];
"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-xsane-0.999.drv" -> "cccccccccccccccccccccccccccccccc-system-path.drv" [color = "black"];
"dddddddddddddddddddddddddddddddd-foo-gui-1.0.drv" -> "cccccccccccccccccccccccccccccccc-system-path.drv" [color = "black"];
"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-gtk2-2.24.33.drv" -> "dddddddddddddddddddddddddddddddd-foo-gui-1.0.drv" [color = "black"];
"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-hello-2.12.drv" -> "cccccccccccccccccccccccccccccccc-system-path.drv" [color = "black"];
"cccccccccccccccccccccccccccccccc-system-path.drv" -> "ffffffffffffffffffffffffffffffff-nixos-system-desktop-26.11.drv" [color = "black"];
}"#;

    #[test]
    fn finds_all_entries() {
        let g = DrvGraph::from_dot(
            "/nix/store/ffffffffffffffffffffffffffffffff-nixos-system-desktop-26.11.drv",
            DOT,
        );
        let t = g.find_by_name("gtk2-2.24.33");
        assert_eq!(t.len(), 1);
        let prov = vec![
            ProvDef {
                option: "environment.systemPackages".into(),
                file: "/x/hosts/desktop.nix".into(),
                names: vec![Some("xsane-0.999".into()), Some("hello-2.12".into())],
            },
            ProvDef {
                option: "environment.systemPackages".into(),
                file: "/nix/store/zz-source/nixos/modules/services/foo.nix".into(),
                names: vec![Some("foo-gui-1.0".into())],
            },
        ];
        let mut e = entries(&g, &t, &prov);
        rank(&mut e, &Ownership::default());
        let names: Vec<&str> = e.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["foo-gui-1.0", "xsane-0.999"]);
        assert_eq!(
            e[1].chain,
            vec![
                "nixos-system-desktop-26.11",
                "system-path",
                "xsane-0.999",
                "gtk2-2.24.33"
            ]
        );
        assert_eq!(tree(&e[1].chain)[3], "      └─ gtk2-2.24.33");
    }
}
