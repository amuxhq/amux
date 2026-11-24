# What Buck2's Windows actions run with, on a Linux worker (flake.nix's
# `packages.x86_64-linux.windows`, realized by //:nix-windows): the pinned
# Rust, LLVM's MSVC-compatible C compiler, linker and librarian, and
# Microsoft's CRT and Windows SDK, which nixpkgs fetches with xwin from the
# Visual Studio manifest it pins. Everything built for Windows is compiled
# and linked here, so a Windows machine only runs what it is given
# (docs/BUILD.md, "Windows").
#
# x64 (x86_64-pc-windows-msvc, what releases ship) is in bin/; Arm64
# (aarch64-pc-windows-msvc, for Windows on Arm, which a Windows VM on an
# Apple Silicon Mac is) in arm64/bin/, beside the same Rust, which carries
# both targets' standard libraries. Each architecture's CRT and SDK are
# their own xwin download.
#
# The tools are wrapped so that every caller (the prelude's C and Rust rules,
# and the `cc` crate in a build script) finds the SDK without being told:
# the compiler is given the SDK's headers and the linker its libraries.
# Apart from `packages.actions`, so that an SDK or LLVM update changes only
# Windows actions' keys.
{ pkgs }:
let
  rust = (pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml).override {
    targets = [ "x86_64-pc-windows-msvc" "aarch64-pc-windows-msvc" ];
  };
  llvm = pkgs.llvmPackages;
  x64 = pkgs.windows.sdk;
  # nixpkgs' SDK is fetched for the platform it is built for; the Arm64 one
  # is the same derivation over an Arm64 download, whose hash nixpkgs pins
  # beside the others.
  msvcSdk = "${pkgs.path}/pkgs/os-specific/windows/msvcSdk";
  arm64 = pkgs.windows.sdk.overrideAttrs (old: {
    src = pkgs.callPackage "${msvcSdk}/fetchWinSdk.nix" { } {
      manifest = "${msvcSdk}/manifest.json";
      arch = "aarch64";
      hash = (builtins.fromJSON (builtins.readFile "${msvcSdk}/hashes.json")).aarch64;
    };
  });
  # clang-cl and lld-link for one architecture, in DIR.
  wrappers = dir: target: sdk: lib: ''
    mkdir -p ${dir}
    cat > ${dir}/clang-cl <<EOF
    #!/bin/sh
    exec ${llvm.clang-unwrapped}/bin/clang-cl --target=${target} /vctoolsdir ${sdk}/crt /winsdkdir ${sdk}/sdk "\$@"
    EOF
    cat > ${dir}/lld-link <<EOF
    #!/bin/sh
    exec ${llvm.lld}/bin/lld-link /libpath:${sdk}/crt/lib/${lib} /libpath:${sdk}/sdk/lib/um/${lib} /libpath:${sdk}/sdk/lib/ucrt/${lib} "\$@"
    EOF
    chmod +x ${dir}/clang-cl ${dir}/lld-link
  '';
in
pkgs.runCommand "amux-windows" { } ''
  mkdir -p $out/bin
  for tool in rustc rustdoc clippy-driver; do
    ln -s ${rust}/bin/$tool $out/bin/$tool
  done
  ln -s ${x64} $out/sdk
  ln -s ${arm64} $out/sdk-arm64
  ${wrappers "$out/bin" "x86_64-pc-windows-msvc" x64 "x64"}
  ${wrappers "$out/arm64/bin" "aarch64-pc-windows-msvc" arm64 "arm64"}
  for tool in llvm-lib llvm-ar llvm-nm llvm-objcopy llvm-objdump llvm-strip llvm-ranlib llvm-rc; do
    ln -s ${llvm.llvm}/bin/$tool $out/bin/$tool
  done
''
