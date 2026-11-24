# The tools amux development needs, defined once. The dev shell uses this list,
# and so does `packages.toolchain`, which installs it as a profile -- on a
# machine where the tools should be on every shell's PATH, not only inside
# `nix develop`. A machine definition can install the same list.
#
# `buildTools = false` leaves out build/tools.json's tools: that is the
# list Buck2's Linux actions run with (flake.nix's `packages.actions`), and
# a new buck2 or NativeLink shouldn't change the key of every Linux action.
{ pkgs, buildTools ? true }:
let
  # The stable toolchain, read from rust-toolchain.toml so that rustup users and
  # this list cannot disagree about the version, components or targets.
  rust = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;

  # The dated nightly that `just fmt`, `just fmt-check`, `just miri` and
  # `just sanitize` use.
  # Keep the date in step with the justfile's `nightly_cargo`.
  nightly = pkgs.rust-bin.nightly."2026-08-30".minimal.override {
    extensions = [ "miri" "rust-src" "rustfmt" ];
  };

  # Stands in for rustup's `cargo +nightly-…`, which needs rustup; the justfile
  # uses it whenever it is on PATH. The whole nightly goes first on PATH, not
  # just its cargo: cargo finds rustfmt, rustc and cargo-miri on PATH, and
  # would otherwise reach the stable ones.
  cargoNightly = pkgs.writeShellScriptBin "cargo-nightly" ''
    PATH=${nightly}/bin:$PATH exec cargo "$@"
  '';

  pinned = import ./build-tools.nix { inherit pkgs; };
in
[
  rust
  cargoNightly
  pkgs.cargo-deny
  pkgs.cargo-shear
  pkgs.git
  pkgs.gitleaks
  pkgs.just
  pkgs.buf
  pkgs.python3
  pkgs.tmux
  # The browser extension under js/: its build scripts, npm and its tests.
  pkgs.nodejs_24
  # The Android app under apps/android/: Gradle/Kotlin builds, JVM tests and Rust JNI cross-compilation.
  pkgs.jdk21
  pkgs.cargo-ndk
]
# A C compiler for the crates that build C: ring, and SQLite where it is
# bundled. Linux only. On macOS the iPhone bridge needs Apple's own toolchain
# from Xcode, and Nix's compiler and SDK variables get in its way.
++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.stdenv.cc ]
# build/tools.json's buck2, reindeer, nativelink and cargo-nextest.
++ pkgs.lib.optionals buildTools (builtins.attrValues pinned.tools)
