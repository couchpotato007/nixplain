# olm reaches the system through a user's home-manager configuration.
{
  users.users.alice.isNormalUser = true;
  home-manager.useGlobalPkgs = true;
  home-manager.users.alice = import ../home/alice.nix;
}
