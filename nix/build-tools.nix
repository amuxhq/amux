# The build tools build/tools.json pins (buck2, reindeer, nativelink), as Nix
# packages: the same release assets and SHA-256 digests scripts/buck-tools.py
# fetches, so one file holds each version for both.
#
# `cache` lays them out as scripts/buck-tools.py's cache does
# (NAME/VERSION/NAME). The dev shell points AMUX_TOOLS_CACHE at it, so
# scripts/buck2 inside `nix develop` runs these and never downloads.
{ pkgs }:
let
  inherit (pkgs) lib stdenv;
  pins = lib.filterAttrs (name: _: !(lib.hasPrefix "/" name))
    (builtins.fromJSON (builtins.readFile ../build/tools.json));
  triple = {
    "x86_64-linux" = "x86_64-unknown-linux-gnu";
    "aarch64-darwin" = "aarch64-apple-darwin";
  }.${stdenv.hostPlatform.system} or null;

  tool = name: pin:
    let asset = pin.assets.${triple}; in
    stdenv.mkDerivation {
      pname = name;
      inherit (pin) version;
      src = pkgs.fetchurl { inherit (asset) url sha256; };
      dontUnpack = true;
      # The Linux releases link glibc and libgcc dynamically; point them at
      # Nix's so they don't depend on the host's.
      nativeBuildInputs = [ pkgs.zstd ] ++ lib.optionals stdenv.isLinux [ pkgs.autoPatchelfHook ];
      buildInputs = lib.optionals stdenv.isLinux [ stdenv.cc.cc.lib pkgs.zlib ];
      installPhase = ''
        mkdir -p $out/bin
        ${if lib.hasSuffix ".zst" asset.url then ''
          zstd -q -d $src -o $out/bin/${name}
        '' else ''
          tar -xzf $src -C $out/bin ${asset.member}
          [ ${asset.member} = ${name} ] || mv $out/bin/${asset.member} $out/bin/${name}
        ''}
        chmod 0755 $out/bin/${name}
      '';
    };

  tools = if triple == null then { } else lib.mapAttrs tool pins;
in
{
  inherit tools;
  cache = pkgs.linkFarm "amux-build-tools" (lib.mapAttrsToList
    (name: drv: { name = "${name}/${drv.version}/${name}"; path = "${drv}/bin/${name}"; })
    tools);
}
