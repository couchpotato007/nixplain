{
  description = "nixplain demo: one NixOS host or package per error nixplain explains";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.home-manager = {
    url = "github:nix-community/home-manager";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { nixpkgs, home-manager, ... }:
    let
      inherit (nixpkgs) lib;
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};

      # Every host is the minimal base system plus one broken module (or a
      # list of modules).
      host = module: lib.nixosSystem {
        inherit system;
        modules = [ ./hosts/base.nix ] ++ lib.toList module;
      };
    in
    {
      # nixplain os --host <name>
      nixosConfigurations = lib.mapAttrs (_: host) {
        # A: meta checks
        meta-insecure = ./hosts/meta-insecure.nix; # via systemPackages and a user's packages
        meta-insecure-service = ./hosts/meta-insecure-service.nix; # via a NixOS service
        meta-unfree = ./hosts/meta-unfree.nix;
        meta-broken = ./hosts/meta-broken.nix; # marked broken in your own overlay
        meta-unsupported = ./hosts/meta-unsupported.nix; # darwin-only package
        meta-non-source = ./hosts/meta-non-source.nix; # binary package with allowNonSource = false
        meta-insecure-home = [ # via home-manager's home.packages (NixOS module)
          home-manager.nixosModules.home-manager
          ./hosts/meta-insecure-home.nix
        ];

        # B: evaluation
        eval-type = ./hosts/eval-type.nix; # lib.mapAttrs on a list
        eval-recursion-imports = ./hosts/eval-recursion-imports.nix; # config used in imports
        eval-recursion = ./hosts/eval-recursion.nix; # option defined through itself
        eval-missing-attribute = ./hosts/eval-missing-attribute.nix; # pkgs.helo
        eval-undefined-variable = ./hosts/eval-undefined-variable.nix; # with pkgs; [ helloo ]
        eval-missing-argument = ./hosts/eval-missing-argument.nix; # own helper function
        eval-unexpected-argument = ./hosts/eval-unexpected-argument.nix;
        eval-callpackage = ./hosts/eval-callpackage.nix; # callPackage can't fill an argument
        eval-syntax = ./hosts/eval-syntax.nix;
        eval-throw = ./hosts/eval-throw.nix; # throw in your own code

        # C: module system
        module-unknown-option = ./hosts/module-unknown-option.nix; # typo, Nix suggests a fix
        module-unknown-option-nosuggest = ./hosts/module-unknown-option-nosuggest.nix;
        module-conflict = ./hosts/module-conflict.nix; # hostName set in two files
        module-type = ./hosts/module-type.nix; # string in a list of ports
        module-undefined = ./hosts/module-undefined.nix; # fileSystems entry without fsType
        module-assertion = ./hosts/module-assertion.nix; # failed NixOS assertion

        # D: a nixpkgs package fails to build inside your system
        build-in-system = ./hosts/build-in-system.nix;
      };

      # nixplain home --user <name>
      homeConfigurations.alice = home-manager.lib.homeManagerConfiguration {
        inherit pkgs;
        modules = [ ./home/alice.nix ./home/standalone.nix ];
      };

      # nixplain build .#<name>
      packages.${system} = {
        # A: meta check through a plain nix build
        meta-insecure-package = pkgs.nheko;

        # D: build failures (nixplain shows the right log lines, not build-system specific advice)
        build-configure = pkgs.callPackage ./pkgs/build-configure.nix { }; # meson: dependency missing
        build-compile = pkgs.callPackage ./pkgs/build-compile.nix { }; # C: header missing
        build-tests = pkgs.callPackage ./pkgs/build-tests.nix { };
        build-no-output = pkgs.callPackage ./pkgs/build-no-output.nix { };
        build-no-message = pkgs.callPackage ./pkgs/build-no-message.nix { }; # fails without an error line

        # E: hash mismatches (try `nixplain --fix build .#<name>`)
        fetch-placeholder = pkgs.callPackage ./pkgs/fetch-placeholder.nix { }; # "sha256-AAAA…" string
        fetch-fakehash = pkgs.callPackage ./pkgs/fetch-fakehash.nix { }; # lib.fakeHash expression
        fetch-outdated = pkgs.callPackage ./pkgs/fetch-outdated.nix { };
        fetch-nix32 = pkgs.callPackage ./pkgs/fetch-nix32.nix { };
        fetch-cargo = pkgs.callPackage ./pkgs/fetch-cargo.nix { };
      };
    };
}
