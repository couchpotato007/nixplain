# A home-manager configuration. nheko needs the insecure olm.
{ pkgs, ... }:
{
  home.stateVersion = "26.05";
  home.packages = with pkgs; [
    jq
    nheko
  ];
}
