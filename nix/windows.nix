# What Buck2's Windows actions run with, on a Linux worker (flake.nix's
# `packages.x86_64-linux.windows`, realized by //:nix-windows): the pinned
# Rust, LLVM's MSVC-compatible C compiler, linker and librarian, and
# Microsoft's CRT and Windows SDK, which nixpkgs fetches with xwin from the
# Visual Studio manifest it pins. Everything built for x86_64-pc-windows-msvc
# is compiled and linked here, so a Windows machine only runs what it is
# given (docs/BUILD.md, "Windows").
#
# The tools are wrapped so that every caller (the prelude's C and Rust rules,
# and the `cc` crate in a build script) finds the SDK without being told:
# the compiler is given the SDK's headers and the linker its libraries.
# Apart from `packages.actions`, so that an SDK or LLVM update changes only
# Windows actions' keys.
{ pkgs }:
let
  rust = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;
  sdk = pkgs.windows.sdk;
  llvm = pkgs.llvmPackages;
  target = "x86_64-pc-windows-msvc";
in
pkgs.runCommand "amux-windows" { } ''
  mkdir -p $out/bin
  for tool in rustc rustdoc clippy-driver; do
    ln -s ${rust}/bin/$tool $out/bin/$tool
  done
  ln -s ${sdk} $out/sdk

  cat > $out/bin/clang-cl <<EOF
  #!/bin/sh
  exec ${llvm.clang-unwrapped}/bin/clang-cl --target=${target} /vctoolsdir ${sdk}/crt /winsdkdir ${sdk}/sdk "\$@"
  EOF
  cat > $out/bin/lld-link <<EOF
  #!/bin/sh
  exec ${llvm.lld}/bin/lld-link /libpath:${sdk}/crt/lib/x64 /libpath:${sdk}/sdk/lib/um/x64 /libpath:${sdk}/sdk/lib/ucrt/x64 "\$@"
  EOF
  chmod +x $out/bin/clang-cl $out/bin/lld-link
  for tool in llvm-lib llvm-ar llvm-nm llvm-objcopy llvm-objdump llvm-strip llvm-ranlib llvm-rc; do
    ln -s ${llvm.llvm}/bin/$tool $out/bin/$tool
  done
''
