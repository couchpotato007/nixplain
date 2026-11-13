# nixplain: Implementation Status

Draft · 13 Nov 2026 · prototype v0.1.0 · updated after extending the demo

This document compares the prototype with the design in "nixplain: Better
Error Messages for Nix". It lists what is implemented, what works but needs
more work, and what could come later. Everything listed as implemented was
tested against real Nix output (CppNix 2.34, nixpkgs-unstable of Nov 2026),
not only against mock-ups.

## Scope decisions

- **CppNix only.** Lix is out of scope, so no Lix fixtures or Lix-specific
  parsing.
- **Flakes only.** Channels and `NIX_PATH` setups are not supported.
- **Rust.** No Python prototype. The CLI is a single binary built with
  `cargo build` or `nix build`.
- **The primary marker goes where the user edits.** Nix often reports where
  it *noticed* a problem (a function definition, the token after a missing
  `;`). nixplain puts `^^^` on the line the user has to change and shows the
  other place only as context.
- **No build-system knowledge base.** The design proposed a registry of
  build-log patterns per build system (meson, cmake, python, …). That would
  need every way of building a package registered and is out of scope. The
  build engine stays generic: the right log lines, the phase, ownership and
  the comparison with the last good generation. The missing dependency is
  usually obvious from the error line. (A registry was prototyped and
  removed again.)

## Architecture as built

```
Runner → Classifier → Context → Engine (per class) → Renderer
runner.rs  engines/mod.rs  context.rs  engines/*.rs      render.rs
           nixlog.rs       graph.rs
```

| Design component | File | State |
|---|---|---|
| Runner (`--log-format internal-json --show-trace`) | `runner.rs`, `nixlog.rs` | done |
| Classifier (classes A–E) | `engines/mod.rs` | done |
| Ownership map | `context.rs` | done |
| Provenance index | `graph.rs` | done for system, user and home-manager packages |
| Source lookups on the syntax tree | `nixast.rs` (rnix) | done |
| Search scope = the configuration's modules | `context.rs` | done |
| Last good generation | `context.rs`, `engines/build.rs` | done |
| Diagnostic second run via `extendModules` | `graph.rs` | done |
| Renderer: human, `--short`, `--json`, `explain` | `render.rs`, `explain.rs` | done |
| Knowledge base for build logs | — | dropped (see scope) |

## Implemented

### Runner and log parsing

- Runs `nix build` with `--log-format internal-json --show-trace` and shows a
  one-line progress display while building.
- Parses errors with their structured trace frames (file, line, column,
  message). The innermost frame comes first.
- Collects build activities: the derivation, the current phase (from
  `setPhase` events) and the log lines.
- Filters out warnings that Nix logs at error level (for example an
  unreachable substituter) and passes them through to the user.
- Drops follow-up errors such as "1 dependency failed" when the real error
  is also in the log.
- `--save-log FILE` keeps the raw log. `nixplain diagnose FILE` replays it
  offline, which is useful for bug reports and fixtures.

### Context layer

- **Ownership map:** uses `nix flake archive --json --dry-run` to map
  `/nix/store/<hash>-source` paths back to the local checkout (the git root
  for git flakes) and to input names such as `<nixpkgs>/lib/attrsets.nix`.
  Unmapped store paths with a nixpkgs layout are labelled as nixpkgs.
- **Provenance:** reads `definitionsWithLocations` of
  `environment.systemPackages`, `users.users.<name>.packages` and, for
  home-manager as a NixOS module, every user's `home.packages` (through
  `options.home-manager.users.valueMeta.attrs.<name>.configuration`, which
  nixpkgs 25.11+ exposes for submodules). Standalone home-manager
  (`nixplain home`) reads `homeConfigurations.<user>.options.home.packages`;
  its package environment `home-manager-path` is treated like `system-path`.
- **Search scope:** option, service, override and `broken = true` lookups
  only search the user's files that are modules of the failed configuration,
  taken from `nixosConfigurations.<host>.graph` (or
  `homeConfigurations.<user>.graph`), evaluated once, on first use (about 3
  seconds). A flake with several hosts no longer finds another host's line;
  checked on the demo with a decoy file. When the graph cannot be evaluated
  (broken `imports`, syntax errors, old nixpkgs, `--no-rerun`) nixplain falls
  back to every `.nix` file. Package definitions and hashes live in files
  loaded with `callPackage`, which are not modules, so they are still
  searched in the whole flake.
- **Source lookups (rnix):** every location in the user's code is found on
  the syntax tree, not with regexes. `nixast::defs()` lists every binding
  with its full attribute path through nested sets, `mkIf`/`mkMerge`,
  `let`, `with`, lambdas, lists and `//`; a leading `config.` is ignored.
  On top of that: option definitions (exact, or the definitions below an
  option), list elements of an option (`with pkgs; [ nheko ]`,
  `[ pkgs.a ] ++ [ … ]`), string literals (hashes), the `version` next to a
  hash, arguments in a call, a function's argument pattern, `callPackage`
  argument patterns and application parts (`lib.optional cond ./x.nix`).
  Option paths also match by suffix, for modules imported at a nested
  level (a home-manager module defining `home.packages`). Comments and
  strings can no longer produce false matches. The regexes that remain only
  parse Nix's own messages.
- **Last good generation:** the derivation of
  `/nix/var/nix/profiles/system`.

### A: Meta (insecure, unfree, broken, unsupported, non-source, blocklisted)

- Parses the package, the kind of problem and the reason, including the new
  `meta.problems` format ("has problems: - broken: …").
- Diagnostic re-run: `builtins.getFlake` +
  `nixosConfigurations.<host>.extendModules` with relaxed `nixpkgs.config`.
  The `NIXPKGS_ALLOW_*` variables are set as well, as a fallback for when
  `nixpkgs.pkgs` is set externally.
- Derivation graph from `nix-store --query --graph`. Finds every entry point:
  - packages under `system-path`
  - per-user profiles (`etc → user-environment → pkg`)
  - systemd units (`etc → system-units → unit-X.service`), mapped back to the
    user's `services.X…` line by matching prefixes of the unit name
    (`pantalaimon-matrix` → `services.pantalaimon-headless`)
- Paths from other toplevel children must avoid `system-path`, so the same
  package isn't reported twice through `etc` or `activate`.
- Fixes: remove or disable the source, or permit the package (as module code
  for NixOS, as an environment variable for `nix build`).
- Broken: when the user's own overlay sets `meta.broken = true`, that line is
  shown as well.
- Unsupported platform: the reason is shortened to "only for aarch64-darwin
  (this system: x86_64-linux)".
- Non-source: suggests a narrow `allowNonSourcePredicate`. When the rejected
  package is a bootstrap compiler (the usual result of
  `allowNonSource = false`), a note explains why.
- Package lines are matched as uses, not definitions: `cowsay = prev.cowsay…`
  in an overlay is skipped in favour of the `systemPackages` entry.
- Also works for plain installables (`nixplain build .#foo`). There it lists
  the direct dependencies that lead to the package instead of options.

### E: Fetch

- Parses the derivation, the specified hash and the correct hash.
- Searches the user's `.nix` files for the old hash in SRI, nix32, base16 and
  base64 form (converted with `nix hash convert`), and for placeholders also
  for `lib.fakeHash`/`lib.fakeSha256` written as expressions. Comments are
  ignored. When the same old hash appears in several files, it says so.
- Shows the line, plus a nearby `version = …` as context. Recognises
  placeholder hashes (`lib.fakeHash`), and explains that
  `cargoHash`/`vendorHash`/`npmDepsHash` cover vendored dependencies, not the
  source.
- `--fix` replaces the hash in the same encoding (a `lib.fakeHash`
  expression becomes a string). Checked on the demo: SRI, nix32 and
  `lib.fakeHash` all build after `--fix`.

### D: Build

- Failing phase from the JSON events, falling back to the "Running phase:"
  lines in the log.
- Fetches the full log with `nix log` when the stream only had a few lines.
- Shows the first line that looks like a real error, with its context (a
  preceding "In function …:" line, the compiler's source excerpt), and how
  many more error lines follow. Without any error line: the last lines and
  the exit code.
- "Built, but did not create `$out`" gets its own diagnosis.
- When a nixpkgs package fails and the user overrides it
  (`hello = prev.hello.overrideAttrs …`), the override is shown as the
  first suspect instead of the Hydra advice.
- Log excerpts are cleaned of ANSI colours, GCC's OSC 8 hyperlinks and
  forwarded `@nix` events.
- Ownership: searches the user's files for `pname = "…"`. For nixpkgs
  packages the fixes are generic: check Hydra, skip the tests with an overlay
  (only for test failures), use the previous nixpkgs revision.
- Last good generation diff (NixOS only): finds the same package in the old
  system closure and compares its inputs by version, plus the toolchain from
  stdenv's `defaultNativeBuildInputs`/`initialPath`. A compiler change shows
  up, for example as `clang-wrapper 21.1.8 added`.
- "pulled in by" chain for NixOS targets.

### C: Module system

- Unknown option, with the "did you mean" from Nix. Points at the exact
  token, including in nested attribute sets (`pulse.enabled` inside
  `services.pipewire = { … }`).
- Conflicting definitions: lists all definitions and suggests `lib.mkForce`.
- Wrong type: shows the value at the definition site.
- Type errors in list entries (`allowedTCPPorts = [ "22" 80 ]`): the
  internal `."[definition 1-entry 1]"` path is turned into "entry 1 of option
  …" and the value itself is underlined.
- "Used but not defined": points at the submodule entry that lacks the field
  (`fileSystems."/data"` without `fsType`).
- Unknown options without a suggestion are still located in the file.
- Failed NixOS assertions (`module/assertion`): every line that sets an
  option named in the message is marked.

### B: Evaluation

- Blame boundary: picks the innermost trace frame in the user's files, and
  notes where the error was actually raised.
- Hints for type errors (`mapAttrs` on a list → `genAttrs`, list ↔ set
  conversions, function used as a value), missing attributes, undefined
  variables, missing or unexpected arguments.
- Infinite recursion: detects `config` in `imports` and options defined
  through themselves (`x = config.x;`) from the source line. Otherwise gives a
  generic hint.
- `pkgs.helo` and `with pkgs; [ helloo ]`: suggests the closest package name
  from the system's pkgs (edit distance, preferring names that contain every
  typed letter).
- Missing or unexpected function arguments: the primary marker is on the
  call (for an unexpected argument on the `shell = …` line inside it); the
  function's argument set is shown as context.
- `callPackage` with an argument that is not in pkgs
  (`eval/callpackage-argument`): points at the argument in the package file
  and lists Nix's suggestions.
- Syntax errors that expect `;`: titled "missing `;` at the end of line N",
  marked where the `;` belongs; the token where Nix noticed it is context.
- `throw`/`abort` in user code gets its own code (`eval/throw`).
- Trace paths of flake inputs in the new `«path:/nix/store/…»/lib/x.nix`
  and `«github:…»/…` forms are mapped to inputs.

### Output and tooling

- rustc-style renderer with multi-label snippets, `⋮` for gaps, trees and
  numbered fixes. Colour only on a terminal, honours `NO_COLOR`.
- `--short`, `--json` (codes, locations, blocks, edits), `--no-rerun`.
- `--show-trace` prints Nix's own error with its full trace before the
  diagnosis; `--show-trace=short` condenses it to one line per frame, maps
  store paths to your file names and marks the frames in your code with
  `▶`. Both go to stdout before a `──── nixplain ────` separator, so the
  order survives pipes. (`--raw` is an alias.)
- `nixplain explain [code]`.
- `nixplain os switch|boot|test` activates like nixos-rebuild: sets the
  system profile and runs `switch-to-configuration`, with sudo when not root.
- `flake.nix` with a package and a dev shell.
- `examples/demo`: 36 cases (24 NixOS hosts, 1 home-manager configuration, 11 packages), one per error
  code and variation, with a `run-all.sh` script and a table of expected
  results. All 36 produce the expected code on Nix 2.34 / nixos-unstable of
  Nov 2026, and every message was reviewed for where it points and whether
  its fix is useful.
- Tests: 29 unit tests (5 of them for the syntax-tree lookups), with fixtures and messages from real Nix 2.34 runs.
  One ignored test needs a real NixOS system profile.

## Needs more work

These parts exist but are approximate or tested only lightly.

### Correctness and robustness

- **Purity of the second run.** It uses `--impure`, required by
  `getFlake` on a local path and by the `NIXPKGS_ALLOW_*` fallback. The
  design asks to keep `--impure` minimal. Locking the flake ref (passing the
  `narHash`) would allow a pure `getFlake`.
- **Merge order of `nixpkgs.config`.** The relaxing module relies on being
  merged last (`recursiveUpdate`, the last definition wins). An explicit
  `allowUnfree = false` from the user could still win. This is not tested.
- **Name matching.** Provenance is matched to graph nodes by derivation name.
  Wrapped or renamed packages (`pkg.override`, `symlinkJoin`, wrappers)
  can fail to match. Locating the line by `pname` fails when the attribute
  name differs from the `pname` (for example `python3Packages.foo` with pname
  `python3.12-foo`).
- **Ownership for git flakes in subdirectories** assumes Nix copies the whole
  repository. Flakes referenced as `path:` inside a git repo may map
  incorrectly.
- **Build ownership** looks for a `pname = "x"` (or `name = "x-…"`)
  binding. Packages whose name is computed differently are not found.
- **Package files are not in the scope.** Files loaded with `callPackage`
  or `import` (not as modules) are invisible to the module graph. Package
  definitions and hashes are therefore still searched in the whole flake,
  and a home-manager module imported by value (`users.alice = import
  ./home.nix`) is found by a fallback search of all files, because Nix
  attributes its definitions to the importing file.
- **Package suggestions** cost an extra evaluation of `attrNames pkgs` and
  only exist for NixOS targets.
- **Dynamic attribute names** (`${name} = …`) and paths built at
  evaluation time (`lib.setAttrByPath`, `mapAttrs` over a list) cannot be
  matched on the syntax tree; such definitions are not located.
- **Multiple systems / cross.** `find_by_name` can find several derivations
  with the same name. They are all treated as targets.
- **Hostname default.** `nixplain os` uses the kernel hostname. With
  `networking.hostName` unset, the toplevel is named `nixos-system-nixos-…`,
  which looks odd in the tree.

### Coverage

- **home-manager** is covered for `home.packages`. Packages that a
  home-manager *module* adds (e.g. `programs.foo.enable`) show up with the
  chain, but without the `programs.*` line that enabled them, and
  home-manager provenance inside NixOS needs nixpkgs 25.11+.
- **Unit name → option** only covers systemd services whose name starts with
  the option name. Timers, sockets, user units, `environment.etc` entries and
  `system.activationScripts` are reported only as "via etc".
- **First-error heuristic.** One regex decides what an error line is. It
  will pick the wrong line for some tools (e.g. a test runner that prints
  "error" in passing output). Needs more real logs to tune.
- **Fetch errors other than hash mismatches** (download failures, 404,
  `fetchgit` revision not found) fall through to a generic diagnostic.
- **Eval hints** are a fixed table in Rust code. That is fine while they are
  few.
- **The baseline diff** compares only direct inputs and stdenv's toolchain,
  not deeper dependencies. "No direct input changed" is common for breakages
  caused deeper in the closure.

### UX

- Global flags must come before the subcommand, because everything after the
  installable is passed to Nix.
- The reason text for insecure packages is cut at 200 characters.
- Progress output is minimal (a single line). It could show the same
  information as nix-output-monitor without diagnosis.
- `nixplain os` does not support `nh` and has no `--target-host` or
  `--build-host`.
- No configuration file (default flake and host, colour).

### Testing

- No end-to-end tests in CI. `examples/demo/run-all.sh` runs all 36 cases by
  hand in about 7 minutes; there are no golden outputs to compare against
  yet.
- Fixtures cover single errors only, not `--keep-going` runs with several
  failures.
- There are no snapshot tests for the full human output of each class.

## Future work

Ordered roughly by value per effort.

1. **Fixture corpus.** Collect real failing logs (from Hydra and from
   users via `--save-log`) and keep one fixture per pattern and error class.
   Add snapshot tests (e.g. `insta`) for the rendered output. The demo is a
   ready source: `LOGS=dir ./run-all.sh` saves one log per case.
2. ~~home-manager provenance~~ (done; `programs.*` that add packages are
   still missing).
3. **Edits on the syntax tree.** Locations already come from rnix. The next
   step is to generate fixes as real text edits from the same tree (remove a
   list element, rename an option, add `;`, add a function argument)
   instead of code to copy. That makes `--fix` possible for more than
   hashes.
4. **Hydra status.** For failing nixpkgs packages, query Hydra for the job
   status and show "also broken on Hydra since …". Also suggest a previous
   nixpkgs revision that built it.
5. **LSP integration.** The `--json` output already has locations and edits.
   A small nixd/nil extension, or a standalone LSP mode, could show
   diagnostics in the editor after a failed rebuild.
6. **lib contracts (class B, stage 2).** Generate contracts from the RFC 145
   doc comments in nixpkgs `lib`, inject a checked `lib` via
   `lib.extend`/`specialArgs` in the second run, and blame the call site.
   Expect to curate many signatures by hand.
7. **Debugger replay (class B, stage 3).** Re-trigger the error with
   `nix eval --debugger`, drive the REPL over a pty (`:bt`, `:env`) and show
   real values ("set is the list [ "alice" "bob" ] from hosts/desktop.nix:22").
   Fragile because REPL output is not a stable interface.
8. **Source rewriting.** Wrap the user's bindings in `addErrorContext` before
   the second run to get provenance for their own values.
9. **Evaluator patch.** A debug-mode side table `Value* → PosIdx` in CppNix
   (estimated 100–300 lines, shipped as an overlay) for real origin tracking
   of every type error.
10. **Upstream improvements.** Suggest in nixpkgs that `check-meta.nix`
    mentions `nix why-depends` and names the attribute path. Suggest in Nix
    that the JSON log keeps ANSI-free messages and doesn't put `line:col`
    into `file`.
11. **Non-flake setups.** Channels and `NIX_PATH`, once flakes are solid.

## Suggested next milestone

Make class A and E reliable enough for daily use:

- ~~home-manager provenance~~, ~~rnix-based lookups~~, ~~search scope~~ (done)
- pure second run with a locked flake ref
- fixture corpus and snapshot tests (item 1)
- `--fix` for list elements and option names (item 3)
- CI that runs the demo flake
