# The tools amux development needs, defined once. The dev shell uses this list,
# and so does `packages.toolchain`, which installs it as a profile -- on a
# machine where the tools should be on every shell's PATH, not only inside
# `nix develop`. A machine definition can install the same list.
{ pkgs }:
let
  # The stable toolchain, read from rust-toolchain.toml so that rustup users and
  # this list cannot disagree about the version, components or targets.
  rust = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;

  # The dated nightly that `just fmt`, `just fmt-check` and `just miri` use.
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
in
[
  rust
  cargoNightly
  pkgs.git
  pkgs.just
  pkgs.python3
  pkgs.tmux
]
# A C compiler for the crates that build C: ring, and SQLite where it is
# bundled. Linux only. On macOS the iPhone bridge needs Apple's own toolchain
# from Xcode, and Nix's compiler and SDK variables get in its way.
++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.stdenv.cc ]
