{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
      package = pkgs.callPackage ./nix/package.nix { };
    in
    {
      packages.x86_64-linux = {
        default = package;
        gmessages-tui = package;
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
