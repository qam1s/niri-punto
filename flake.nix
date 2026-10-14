{
  description = "Keyboard layout corrector for the niri Wayland compositor";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
    in
    {
      packages = forSystems (pkgs: {
        niri-punto = pkgs.callPackage ./packaging/nix/package.nix { };
        default = self.packages.${pkgs.system}.niri-punto;
      });

      overlays.default = _final: prev: {
        niri-punto = prev.callPackage ./packaging/nix/package.nix { };
      };

      nixosModules.niri-punto = ./packaging/nix/nixos.nix;
      nixosModules.default = self.nixosModules.niri-punto;
    };
}
