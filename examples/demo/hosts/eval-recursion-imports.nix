{ lib, config, ... }:
{
  options.my.gui = lib.mkEnableOption "the desktop";
  imports = lib.optional config.my.gui ./gui.nix;
}
