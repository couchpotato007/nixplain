{ lib, config, ... }:
{
  options.my.userList = lib.mkOption {
    type = lib.types.listOf lib.types.str;
    default = [ "alice" "bob" ];
  };

  config.users.users = lib.mapAttrs (name: _: { isNormalUser = true; }) config.my.userList;
}
