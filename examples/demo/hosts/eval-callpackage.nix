{ pkgs, ... }:
{
  environment.systemPackages = [ (pkgs.callPackage ../pkgs/needs-unknown-arg.nix { }) ];
}
