{ config, lib, pkgs, modulesPath, ... }:
{
  imports = [ (modulesPath + "/profiles/qemu-guest.nix") ];
  system.stateVersion = "26.05";
  networking.hostName = "demodex-work";
  networking.useDHCP = lib.mkDefault true;
  boot.kernelParams = [ "console=ttyS0,115200" ];
  boot.kernelModules = [ "qemu_fw_cfg" ];
  boot.loader.grub = { enable = true; devices = [ "/dev/vda" ]; };
  fileSystems."/" = { device = "/dev/disk/by-label/nixos"; fsType = "ext4"; autoResize = true; };
  image.modules.qemu.virtualisation.diskSize = 16384;
  users.users.worker = { isNormalUser = true; extraGroups = [ "wheel" ]; };
  security.sudo.wheelNeedsPassword = false;
  services.openssh = {
    enable = true;
    settings = { PasswordAuthentication = false; KbdInteractiveAuthentication = false; PermitRootLogin = "no"; };
  };
  nix.settings.experimental-features = [ "nix-command" "flakes" ];
  nix.nixPath = [ "nixpkgs=${pkgs.path}" "nixos-config=/etc/nixos/configuration.nix" ];
  environment.systemPackages = with pkgs; [ git curl vim tmux ];
  # Provision only a public key, using QEMU's firmware configuration device.
  systemd.services.demodex-identity = {
    wantedBy = [ "multi-user.target" ];
    before = [ "sshd.service" ];
    after = [ "systemd-modules-load.service" ];
    serviceConfig = { Type = "oneshot"; RemainAfterExit = true; };
    path = [ pkgs.coreutils ];
    script = ''
      install -d -m 700 -o worker -g users /home/worker/.ssh
      install -m 600 -o worker -g users /sys/firmware/qemu_fw_cfg/by_name/opt/demodex/authorized-key/raw /home/worker/.ssh/authorized_keys
      install -d -m 755 -o worker -g users /workspace
    '';
  };
  # Seed writable configuration once. Agent edits survive subsequent activations.
  system.activationScripts.demodex-config.text = ''
    mkdir -p /etc/nixos
    if [ ! -e /etc/nixos/configuration.nix ]; then
      cp ${./guest.nix} /etc/nixos/guest.nix
      printf '{ ... }: { imports = [ ./guest.nix ]; }\n' > /etc/nixos/configuration.nix
      chmod 644 /etc/nixos/configuration.nix /etc/nixos/guest.nix
    fi
  '';
}
