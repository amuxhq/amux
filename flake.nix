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
      packages = nixpkgs.lib.recursiveUpdate (forAllSystems (pkgs: {
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
      } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
        # The Chromium the browser extension's end-to-end test drives on a
        # Linux worker (js/BUCK), and a font configuration for it: a worker
        # has no fonts, and Chromium without one lays no text out. Apart from
        # `actions`, so that a browser update changes only that test's key.
        browser = pkgs.runCommand "amux-browser" { } ''
          mkdir -p $out/bin $out/etc
          ln -s ${pkgs.chromium}/bin/chromium $out/bin/chromium
          ln -s ${pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; }} $out/etc/fonts.conf
        '';
      })) {
        # What Buck2's Android actions run with (nix/android.nix), on Linux
        # workers only. Google's Android SDK is unfree, under a licence this
        # import accepts.
        x86_64-linux.android = import ./nix/android.nix {
          pkgs = import nixpkgs {
            system = "x86_64-linux";
            config = { allowUnfree = true; android_sdk.accept_license = true; };
          };
        };
        # What Buck2's Windows actions run with (nix/windows.nix), on Linux
        # workers only. Microsoft's CRT and Windows SDK are unfree, under
        # the Visual Studio licence this import accepts.
        x86_64-linux.windows = import ./nix/windows.nix {
          pkgs = import nixpkgs {
            system = "x86_64-linux";
            overlays = [ rust-overlay.overlays.default ];
            config = { allowUnfree = true; microsoftVisualStudioLicenseAccepted = true; };
          };
        };
      };

      devShells = forAllSystems (pkgs: {
        # NoCC on macOS for the reason given in nix/toolchain.nix.
        default = (if pkgs.stdenv.isDarwin then pkgs.mkShellNoCC else pkgs.mkShell) {
          packages = import ./nix/toolchain.nix { inherit pkgs; };
          # scripts/buck-tools.py finds the pinned tools here rather than
          # downloading them.
          AMUX_TOOLS_CACHE = (import ./nix/build-tools.nix { inherit pkgs; }).cache;
        };
        # What CI's Linux gate runs `just premerge` with when the build
        # host's worker runs its actions: the runner only drives Buck2, and
        # every check that needs Rust's tools is an action on that worker
        # (build/cargo.bzl). scripts/buck-tools.py fetches the pinned buck2
        # (build/tools.json), which zstd unpacks.
        ci = pkgs.mkShellNoCC {
          packages = [ pkgs.gitMinimal pkgs.just pkgs.python3 pkgs.zstd ];
        };
      });
    };
}
