# pantalaimon is insecure. It is not in any package list: the service module
# puts it into a systemd unit.
{
  services.pantalaimon-headless.instances.matrix = {
    homeserver = "https://matrix.org";
  };
}
