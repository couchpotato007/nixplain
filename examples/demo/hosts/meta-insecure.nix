# olm is insecure. It is pulled in twice: by nheko in systemPackages and by
# quaternion in a user's packages. Nix only says "olm is insecure".
{ pkgs, ... }:
{
  environment.systemPackages = with pkgs; [
    hello
    nheko
  ];

  users.users.alice = {
    isNormalUser = true;
    packages = [ pkgs.quaternion pkgs.jq ];
  };
}
