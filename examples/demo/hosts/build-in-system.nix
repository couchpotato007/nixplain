# A nixpkgs package that does not build (simulated with an overlay; in real
# life this is an upstream breakage after `nix flake update`).
{ pkgs, ... }:
{
  nixpkgs.overlays = [
    (final: prev: {
      hello = prev.hello.overrideAttrs {
        postConfigure = ''
          echo "src/hello.c:42:5: error: 'program_name' undeclared (first use in this function)" >&2
          exit 1
        '';
      };
    })
  ];

  environment.systemPackages = [ pkgs.hello ];
}
