{
  lib,
  rustPlatform,
}:

let
  cargoToml = lib.importTOML ./Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = cargoToml.package.name;
  inherit (cargoToml.package) version;

  # Only what cargo needs: keeps examples/, docs/ and target/ out of the
  # store and avoids rebuilds when they change. tests/fixtures is pulled in
  # by include_str! in the unit tests.
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
      ./tests
    ];
  };

  cargoLock.lockFile = ./Cargo.lock;

  # nixplain calls `nix`, `nix-store` and `git` from PATH on purpose: it has
  # to talk to the same Nix (and daemon) the user runs, so they are not
  # wrapped in here.

  meta = {
    description = "Better error messages for Nix";
    mainProgram = "nixplain";
    platforms = lib.platforms.unix;
  };
}
