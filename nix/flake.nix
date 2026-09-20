{
  description = "Demodex isolated work environments";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  outputs = { self, nixpkgs }: {
    nixosConfigurations.work = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [ ./guest.nix ];
    };
    packages.x86_64-linux.vm-image = self.nixosConfigurations.work.config.system.build.images.qemu;
  };
}
