{
  description = "nixplain: better error messages for Nix";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # Build against the consumer's nixpkgs: `pkgs.nixplain` after
      # `nixpkgs.overlays = [ nixplain.overlays.default ];`
      overlays.default = final: _prev: {
        nixplain = final.callPackage ./package.nix { };
      };

      packages = forAllSystems (pkgs: rec {
        nixplain = pkgs.callPackage ./package.nix { };
        default = nixplain;
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };
      });

      checks = forAllSystems (pkgs: {
        inherit (self.packages.${pkgs.stdenv.hostPlatform.system}) nixplain;
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
          packages = with pkgs; [
            clippy
            rustfmt
            rust-analyzer
          ];
        };
      });
    };
}
