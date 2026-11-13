{ pkgs, ... }:
{
  environment.systemPackages = with pkgs; [ helloo ];
}
