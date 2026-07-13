{
  description = "A Gtk4 backend for xdg-desktop-portal";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    flake-compat = {
      url = "github:edolstra/flake-compat";
      flake = false;
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      perSystem =
        {
          config,
          self',
          inputs',
          pkgs,
          system,
          ...
        }:
        {
          packages.xdg-desktop-portal-gtk4 = pkgs.callPackage ./contrib/build.nix { };
          packages.default = config.packages.xdg-desktop-portal-gtk4;

          devShells.default = pkgs.mkShell {
            inputsFrom = [ config.packages.default ];
            buildInputs = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              dbus
            ];
          };
        };
    };
}
