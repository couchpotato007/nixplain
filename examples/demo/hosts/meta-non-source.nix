# Only allow packages built from source; sbt ships prebuilt bytecode.
{ pkgs, ... }:
{
  nixpkgs.config.allowNonSource = false;
  environment.systemPackages = [ pkgs.sbt ];
}
