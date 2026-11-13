# nixplain demo

A flake that is broken on purpose: each NixOS host and each package triggers
one error that nixplain explains. Every host is `hosts/base.nix` (a minimal,
working system) plus one broken module, so each case fails for exactly one
reason.

## Run it

```sh
cargo build                    # in the repository root
cd examples/demo
git add -A .                   # git flakes only see tracked files

./run-all.sh                   # every case, one line each
./run-all.sh full              # every case, full output
./run-all.sh full meta-        # only cases whose name contains "meta-"

../../target/debug/nixplain os --host meta-insecure
../../target/debug/nixplain home --user alice            # standalone home-manager
../../target/debug/nixplain build .#build-gcc14 --no-link
../../target/debug/nixplain --fix build .#fetch-outdated   # rewrites the hash
```

The first run downloads nixpkgs and builds a few tiny packages. A full run
takes several minutes, mostly for the meta cases (they evaluate the system a
second time) and the build cases.

## Cases

### A: meta checks (`nixplain os --host …`, one `nix build`)

| case | code | what it shows |
|---|---|---|
| `meta-insecure` | `meta/insecure` | olm is pulled in twice: by nheko in `environment.systemPackages` and by quaternion in `users.users.alice.packages`. Both lines are shown. |
| `meta-insecure-service` | `meta/insecure` | The insecure package is in no package list. The `services.pantalaimon-headless` module puts it into a systemd unit; nixplain points at the service. |
| `meta-unfree` | `meta/unfree` | Suggests `allowUnfreePredicate` for exactly this package. |
| `meta-broken` | `meta/broken` | Your own overlay sets `meta.broken = true`; nixplain shows both the package line and the overlay. |
| `meta-unsupported` | `meta/unsupported` | A macOS-only package on Linux, with the platform list shortened. |
| `meta-insecure-home` | `meta/insecure` | home-manager as a NixOS module: nheko is in alice's `home.packages` (`home/alice.nix`, imported with `import`); the chain goes through her home-manager service. |
| `home-alice` (`nixplain home --user alice`) | `meta/insecure` | The same with standalone home-manager (`homeConfigurations.alice`). |
| `meta-non-source` | `meta/non-source` | `allowNonSource = false` rejects rust's bootstrap compiler, which most of the system needs to build. Explains why and suggests a narrow predicate. |
| `.#meta-insecure-package` | `meta/insecure` | The same check through a plain `nix build`: dependency chain and an environment-variable fix instead of module code. |

### B: evaluation errors

| case | code | what it shows |
|---|---|---|
| `eval-type` | `eval/type` | `lib.mapAttrs` on a list: points at your call, not at nixpkgs lib, and suggests `lib.genAttrs`. |
| `eval-recursion-imports` | `eval/infinite-recursion` | `config` used in `imports`. |
| `eval-recursion` | `eval/infinite-recursion` | An option defined through itself. |
| `eval-missing-attribute` | `eval/missing-attribute` | `pkgs.helo`: suggests `pkgs.hello`. |
| `eval-undefined-variable` | `eval/undefined-variable` | `with pkgs; [ helloo ]`: suggests `hello`. |
| `eval-missing-argument` | `eval/missing-argument` | Your helper function called without `uid`; marks the function and the call. |
| `eval-unexpected-argument` | `eval/unexpected-argument` | The same helper called with an extra `shell`. |
| `eval-callpackage` | `eval/callpackage-argument` | `callPackage` cannot find `libfoo`; points at the argument in the package file and lists Nix's suggestions. |
| `eval-syntax` | `eval/syntax` | A missing `;`: points at the unexpected token *and* the end of the line before. |
| `eval-throw` | `eval/throw` | A `throw` in your own code. |

### C: module system

| case | code | what it shows |
|---|---|---|
| `module-unknown-option` | `module/unknown-option` | `pulse.enabled` inside a nested attribute set, with "did you mean `enable`?". |
| `module-unknown-option-nosuggest` | `module/unknown-option` | An option without any close match. |
| `module-conflict` | `module/conflict` | `networking.hostName` set in two files; shows both and the `mkForce` fix. |
| `module-type` | `module/type` | A string in a list of ports. |
| `module-undefined` | `module/undefined` | A `fileSystems` entry without `fsType`; points at the entry. |
| `module-assertion` | `module/assertion` | A failed NixOS assertion; marks the lines that set the options it names. |

### D: build failures (`nixplain build .#…`, one host)

| case | code | what it shows |
|---|---|---|
| `build-meson-pkg-config` | `build/configure` | meson can't find libxml2 because pkg-config is missing: fix with both inputs. |
| `build-cmake-package` | `build/configure` | `find_package(fmt)` fails: `buildInputs = [ fmt ]`. |
| `build-gcc14` | `build/compile` | Implicit function declaration, an error since GCC 14. |
| `build-missing-header` | `build/compile` | `zlib.h` not found: `buildInputs = [ zlib ]`. |
| `build-command-not-found` | `build/compile` | `xxd: command not found`: `nativeBuildInputs = [ xxd ]`. |
| `build-tests` | `build/check` | The test suite fails. |
| `build-python-module` | `build/check` | `ModuleNotFoundError` in the import check: `dependencies = [ python3Packages.requests ]`. |
| `build-no-output` | `build/no-output` | meson builds but installs nothing: explains `install : true`. |
| `build-unknown` | `build/compile` | No recognisable error: shows the last log lines. |
| `build-in-system` (host) | `build/configure` | A nixpkgs package fails inside a NixOS system: dependency chain, comparison with your last good generation, and your overlay as the likely cause. |

### E: hash mismatches (`nixplain build .#…`, try `--fix`)

| case | code | what it shows |
|---|---|---|
| `fetch-placeholder` | `fetch/hash` | `hash = "sha256-AAAA…"` |
| `fetch-fakehash` | `fetch/hash` | `hash = lib.fakeHash;` as an expression; `--fix` replaces it with a string. |
| `fetch-outdated` | `fetch/hash` | The version was bumped, the hash was not; marks both lines. |
| `fetch-nix32` | `fetch/hash` | Old-style base32 `sha256`; `--fix` keeps the encoding. |
| `fetch-cargo` | `fetch/hash` | `cargoHash` is wrong; explains that it covers the vendored crates, not the source. |

## Notes

- Results depend on nixpkgs. The cases were checked against nixos-unstable
  of November 2026 (Nix 2.34). When a package stops being insecure or gets a
  different dependency, a meta case changes; the first run writes a
  `flake.lock`, so this only happens on `nix flake update`.
- The fetch cases download small GNU tarballs. `fetch-outdated` and
  `fetch-nix32` contain the hash of the *previous* release on purpose.
- Lookups for options, services and overrides only search the modules of
  the configuration that failed (its module graph), so the many hosts in
  this one flake do not get in each other's way. Package definitions and
  hashes (in files loaded with `callPackage`) are searched in the whole
  flake.
