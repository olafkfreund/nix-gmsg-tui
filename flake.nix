{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
      package = pkgs.callPackage ./nix/package.nix { };
      plugin = pkgs.callPackage ./nix/plugin.nix { };
    in
    {
      packages.x86_64-linux = {
        default = package;
        gmessages-tui = package;
        inherit plugin;
      };

      overlays.default = final: _prev: {
        gmessages-tui = final.callPackage ./nix/package.nix { };
        gmessages-plugin = final.callPackage ./nix/plugin.nix { };
      };

      homeManagerModules.default = import ./nix/hm-module.nix self;

      checks.x86_64-linux = {
        gmessages-tui = package;
        inherit plugin;
      };

      devShells.x86_64-linux.default = pkgs.mkShell {
        inputsFrom = [ package ];
        packages = [
          pkgs.rustc
          pkgs.cargo
          pkgs.clippy
          pkgs.rustfmt
          pkgs.rust-analyzer
          pkgs.qt6.qtdeclarative
        ];
      };
    };
}
