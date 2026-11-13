# cargoHash is the hash of the vendored dependencies, not of the source.
{ rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "hello-rs";
  version = "0.1.0";
  src = ./src/hello-rs;
  cargoHash = "sha256-uKhUOwX3yRF1YBdvbIEpMmZ4JolqI03aswjcGbFL0zs=";
}
