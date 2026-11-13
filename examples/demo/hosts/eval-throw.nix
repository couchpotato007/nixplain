{ lib, config, ... }:
let
  port = 80;
in
{
  services.nginx.enable = true;
  networking.firewall.allowedTCPPorts =
    if port < 1024 then throw "port ${toString port} needs root; pick one above 1024" else [ port ];
}
