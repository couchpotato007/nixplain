{ pkgs, ... }:
{
  environment.systemPackages = [ pkgs.hello ]
  services.openssh.enable = true;
}
