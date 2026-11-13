# Your own overlay marks a package as broken (e.g. copied from an issue).
{ pkgs, ... }:
{
  nixpkgs.overlays = [
    (final: prev: {
      cowsay = prev.cowsay.overrideAttrs (old: {
        meta = old.meta // { broken = true; };
      });
    })
  ];

  environment.systemPackages = [ pkgs.cowsay ];
}
