This vibecoded slop only public that i can easly install it on my machines

# nixplain

Better error messages for Nix. Prototype.

nixplain wraps `nix build` and NixOS rebuilds. Nix runs as usual. Only when
something fails does nixplain start an instrumented second run and print a
rustc-style diagnostic: what broke, why it affects you, where in your code, and
a fix written as Nix code.

```
error[meta/insecure]: olm-3.2.16 is marked as insecure
  reason: The libolm end‐to‐end encryption library used in many Matrix
          clients and Jitsi Meet has been deprecated upstream, …

  pulled in by:
    nixos-system-desktop-26.11
    └─ system-path
       └─ nheko-0.12.1
          └─ olm-3.2.16

  olm is needed by 2 packages:
    nheko       ← environment.systemPackages  hosts/desktop.nix:7
    quaternion  ← users.users.alice.packages  hosts/users.nix:5

  nheko comes from your config:
   ┌─ hosts/desktop.nix:7:5
   │
 7 │     nheko
   │     ^^^^^ in environment.systemPackages

  fixes:
    1. remove nheko
    2. remove quaternion
    3. permit olm-3.2.16:
         nixpkgs.config.permittedInsecurePackages = [ "olm-3.2.16" ];
```

Supports CppNix (tested with 2.34). Lix is not supported. Flakes only.

## Install

As a flake input:

```nix
{
  inputs.nixplain = {
    url = "github:OWNER/nixplain";        # or git+file:///path/to/nixplain
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, nixplain, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        ({ pkgs, ... }: {
          # either the flake's package …
          environment.systemPackages = [ nixplain.packages.${pkgs.stdenv.hostPlatform.system}.default ];
          # … or the overlay, which builds against your nixpkgs: pkgs.nixplain
          # nixpkgs.overlays = [ nixplain.overlays.default ];
        })
      ];
    };
  };
}
```

Or ad hoc: `nix run github:OWNER/nixplain -- build .#pkg`.

nixplain uses `nix`, `nix-store` and `git` from `PATH` so that it talks to the
same Nix you run; the package does not bundle them.

## Usage

```sh
nixplain os [build|switch|boot|test] [--flake .] [--host NAME] [NIX_ARGS…]
nixplain home [build|switch] [--flake .] [--user NAME] [NIX_ARGS…]   # standalone home-manager
nixplain build .#pkg [NIX_ARGS…]          # e.g. --no-link -L
nixplain diagnose log.jsonl --flake . --host NAME   # replay a saved log
nixplain explain meta/insecure             # long explanation of a code
```

Global flags go **before** the subcommand, because everything after the
installable is passed to Nix:

| flag                         |                                                                                                                                                                                   |
| ---------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--json`                     | diagnostics as JSON (file/line/column, edits) for CI and editors                                                                                                                  |
| `--short`                    | one line per error                                                                                                                                                                |
| `--fix`                      | apply machine-applicable fixes (currently: outdated hashes)                                                                                                                       |
| `--no-rerun`                 | skip the diagnostic second run, use only the log                                                                                                                                  |
| `--show-trace[=full\|short]` | print Nix's own error with its trace before the diagnosis: `full` exactly as Nix prints it, `short` one line per frame with frames in your files marked `▶` (`--raw` is an alias) |
| `--save-log FILE`            | keep the internal-json log (for `diagnose` and bug reports)                                                                                                                       |

`nixplain os switch` builds the system, then does what nixos-rebuild does: it
sets the system profile and runs `switch-to-configuration` (via sudo when not
root).

## What it diagnoses

| class     | code                                                                                                                                                               | how                                                                                                                                                                                                                                                                                         |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A meta    | `meta/insecure`, `unfree`, `broken`, …                                                                                                                             | re-evaluates through `extendModules` with checks relaxed, walks the drv graph (`nix-store --query --graph`), maps entry points to `environment.systemPackages` / `users.users.*.packages` / home-manager `home.packages` definitions or to the `services.*` that created a systemd unit     |
| B eval    | `eval/type`, `infinite-recursion`, `missing-attribute`, `undefined-variable`, `missing-argument`, `unexpected-argument`, `callpackage-argument`, `syntax`, `throw` | blame boundary: innermost trace frame in _your_ files, the call site for argument errors, the missing `;` for syntax errors, package-name suggestions for `pkgs.helo`, hints for common mistakes (`mapAttrs` on a list, `config` in `imports`, an option defined through itself)            |
| C modules | `module/unknown-option`, `conflict`, `type`, `undefined`, `assertion`                                                                                              | finds the exact line that sets the option (also nested attribute sets and list entries); failed assertions point at the options they name                                                                                                                                                   |
| D build   | `build/configure`, `compile`, `check`, `install`, `no-output`                                                                                                      | the first real error line with context instead of the last 25 lines, the failing phase, your own package or your override of a nixpkgs package, diff against the same package in your last good system generation (incl. the compiler from stdenv). Deliberately no per-build-system rules. |
| E fetch   | `fetch/hash`                                                                                                                                                       | finds the old hash in your files in any encoding (also `lib.fakeHash`), notes cargo/vendor hashes, `--fix` replaces it in the same encoding                                                                                                                                                 |

## Layout

```
src/
  main.rs        CLI, nixos activation, --fix
  runner.rs      runs nix with --log-format internal-json, progress line
  nixlog.rs      parser for the JSON event stream (errors, traces, build logs)
  nixast.rs      source lookups on the Nix syntax tree (rnix): option definitions, list entries, calls, strings
  context.rs     target, ownership map (store path → your file / input), last good generation
  graph.rs       the diagnostic second run + derivation graph queries
  engines/       one engine per error class, plus the classifier (mod.rs)
  diag.rs        diagnostic data model
  render.rs      human / short / JSON output
  explain.rs     texts for `nixplain explain`
tests/fixtures/            real internal-json logs from Nix 2.34
examples/demo/             a flake that triggers every class
```

## Try it

`examples/demo` is a flake with one broken NixOS host or package for every error
nixplain explains (36 cases, see its README):

```sh
cargo build
cd examples/demo
git add -A .                     # git flakes only see tracked files
./run-all.sh                     # every case, one line each
./run-all.sh full eval-          # full output for the eval cases
../../target/debug/nixplain os --host meta-insecure
../../target/debug/nixplain --fix build .#fetch-outdated --no-link
```

Tests: `cargo test`. The baseline-diff test needs a NixOS system profile:
`NIXPLAIN_TEST_DRV=$(nix eval --raw nixpkgs#bashInteractive.drvPath) cargo test -- --ignored`.
`NIXPLAIN_DEBUG=1` prints the dependency chains the meta engine found.

## Known limits

- The diagnostic run uses `--impure` (for `builtins.getFlake` on a local path
  and the `NIXPKGS_ALLOW_*` fallback when `nixpkgs.pkgs` is set).
- home-manager provenance inside NixOS needs nixpkgs 25.11 or newer (`valueMeta`
  on submodules).
- Finding the first error line in a build log is a heuristic (one regex).
- Not built yet from the design: lib contracts, `--debugger` replay, source
  rewriting, the evaluator patch, Hydra status lookup.
