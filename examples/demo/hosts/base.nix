# A minimal system every demo host starts from. Nothing here is broken.
{
  boot.loader.grub.enable = false;
  fileSystems."/" = {
    device = "/dev/sda1";
    fsType = "ext4";
  };
  networking.hostName = "demo";
  system.stateVersion = "26.05";
}
