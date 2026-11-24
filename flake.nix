{
  description = "amux development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, rust-overlay, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      }));
    in
    {
      packages = forAllSystems (pkgs: {
        toolchain = pkgs.buildEnv {
          name = "amux-toolchain";
          paths = import ./nix/toolchain.nix { inherit pkgs; };
        };
        # What Buck2's Linux actions run with: build/toolchains/nix.bzl
        # realizes it on the worker and calls every tool by its store path.
        actions = pkgs.buildEnv {
          name = "amux-actions";
          paths = import ./nix/toolchain.nix { inherit pkgs; buildTools = false; };
        };
      });

      devShells = forAllSystems (pkgs: {
        # NoCC on macOS for the reason given in nix/toolchain.nix.
        default = (if pkgs.stdenv.isDarwin then pkgs.mkShellNoCC else pkgs.mkShell) {
          packages = import ./nix/toolchain.nix { inherit pkgs; };
          # scripts/buck-tools.py finds the pinned tools here rather than
          # downloading them.
          AMUX_TOOLS_CACHE = (import ./nix/build-tools.nix { inherit pkgs; }).cache;
        };
      });
    };
}
