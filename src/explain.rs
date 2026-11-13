//! Long explanations for `nixplain explain <code>`, like `rustc --explain`.

pub const CODES: &[(&str, &str)] = &[
    (
        "meta/insecure",
        r#"A package is marked as insecure (meta.knownVulnerabilities is not empty).

nixpkgs refuses to evaluate it unless you explicitly permit it. The check runs
inside the insecure package itself, so Nix only names that package, not the
package in your configuration that depends on it. nixplain re-evaluates your
system with the check relaxed and walks the derivation graph to find every
package that pulls it in.

Fix it by removing (or replacing) the package that depends on it, or permit
exactly this version:

    nixpkgs.config.permittedInsecurePackages = [ "olm-3.2.16" ];

Permitting is per version: after an update, the error comes back for the new
version on purpose."#,
    ),
    (
        "meta/unfree",
        r#"A package has an unfree license and allowUnfree is not set.

Allow only the packages you need:

    nixpkgs.config.allowUnfreePredicate = pkg:
      builtins.elem (lib.getName pkg) [ "steam-unwrapped" ];

or all unfree packages with `nixpkgs.config.allowUnfree = true;`.
If you set `nixpkgs.pkgs` yourself (e.g. with home-manager's useGlobalPkgs or
a custom pkgs instance), nixpkgs.config is ignored: pass the config where you
import nixpkgs instead."#,
    ),
    (
        "meta/broken",
        r#"A package is marked as broken (meta.broken = true): it is known not to
build or work on this platform. Allowing it usually just moves the failure to
the build. Check Hydra and the nixpkgs issue tracker, or use an older nixpkgs
revision for this package."#,
    ),
    (
        "meta/unsupported",
        r#"The package's meta.platforms does not include your platform (or
meta.badPlatforms does). It will most likely not build."#,
    ),
    (
        "fetch/hash",
        r#"A fixed-output derivation (a download, or vendored dependencies) produced
a different hash than the one written in the Nix code.

Usual causes: you bumped `version` but not `hash`; the upstream tarball was
re-generated; for cargoHash/vendorHash/npmDepsHash, the dependency lock file
changed. nixplain finds the outdated hash in your files (in any encoding) and
`--fix` replaces it with the correct one."#,
    ),
    (
        "build/configure",
        r#"The configure phase failed (cmake, meson, autotools, ...). Almost always a
missing dependency: libraries go into buildInputs, tools that run during the
build (pkg-config, cmake, meson, ninja, wayland-scanner, ...) into
nativeBuildInputs."#,
    ),
    (
        "build/compile",
        r#"The build phase failed. If the package comes from nixpkgs and built before,
compare with your last good generation: a new compiler (e.g. GCC 14 turning
warnings into errors) is the most common cause."#,
    ),
    (
        "build/check",
        r#"The package's tests failed. For your own packages, fix or disable the
failing tests (disabledTests, doCheck = false). For nixpkgs packages, an
overlay with `doCheck = false` is a reasonable temporary workaround."#,
    ),
    (
        "module/unknown-option",
        r#"You set an option that no imported module declares. Either the name is
misspelled, the option was renamed/removed, or the module that declares it is
not imported (e.g. a home-manager option used in a NixOS module)."#,
    ),
    (
        "module/conflict",
        r#"Several modules define the same option with different values and the option
type cannot merge them. Use lib.mkForce (or lib.mkDefault on the other side)
to choose which one wins."#,
    ),
    (
        "eval/type",
        r#"A value of the wrong type reached a function or builtin. Because Nix is lazy,
the error is raised where the value is used, not where it was created.
nixplain shows the innermost location in your own code."#,
    ),
    (
        "eval/infinite-recursion",
        r#"A value depends on itself. In NixOS configurations this is almost always one
of:

  - `imports` depends on `config` or `pkgs`. Imports are resolved before
    config exists. Import unconditionally and use `config = lib.mkIf … { … }`.
  - a module reads an option inside the definition of that same option.
  - an overlay uses `final.foo` to define `foo` (use `prev.foo`)."#,
    ),
];

pub fn explain(code: &str) -> Option<&'static str> {
    CODES.iter().find(|(c, _)| *c == code).map(|(_, t)| *t)
}
