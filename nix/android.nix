# What Buck2's Android actions run with (flake.nix's `packages.android`):
# the JDK, and Google's Android SDK with the platform, build tools and NDK
# the app is built against. build/toolchains/android.bzl calls each tool by
# its path under this output's store path, so a version bump here changes
# every Android action's key.
#
# It is its own output, apart from `packages.actions`, so a change to the
# Rust toolchain doesn't rekey every Kotlin compile, nor an SDK bump every
# Rust one.
#
# The SDK is Google's, under the Android SDK licence, which `pkgs` must be
# imported accepting (flake.nix).
{ pkgs }:
let
  sdk = pkgs.androidenv.composeAndroidPackages {
    platformVersions = [ "36" ];
    buildToolsVersions = [ "36.0.0" ];
    includeNDK = true;
    ndkVersions = [ "28.2.13676358" ];
    includeEmulator = false;
    includeSystemImages = false;
    includeSources = false;
  };
  jdk = pkgs.jdk21;
in
# One directory per tree rather than a merged environment: the SDK's tools
# find their libraries and jars beside themselves, so each tree stays whole.
#   sdk/  the SDK root (build-tools/36.0.0, platforms/android-36, ndk/28.2...)
#   jdk/  JAVA_HOME
#   bin/  the JDK's tools, first on PATH for whatever they start by name
pkgs.runCommand "amux-android" { } ''
  mkdir -p $out/bin
  ln -s ${sdk.androidsdk}/libexec/android-sdk $out/sdk
  ln -s ${jdk}/lib/openjdk $out/jdk
  for tool in java javac jar jlink jmod; do
    ln -s ${jdk}/lib/openjdk/bin/$tool $out/bin/$tool
  done
''
