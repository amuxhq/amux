#!/bin/sh
# Splits a release build of amux into the binary a release ships and the symbols
# it keeps beside it, both named for the release asset:
#
#   release-symbols.sh <target> <asset> <out-dir>
#
# The shipped binary carries no symbols; the symbols file carries the line
# tables, matched to the binary by its build identity (the GNU debuglink on
# Linux, the Mach-O UUID on macOS, the PDB signature on Windows), so a crash
# address from a shipped binary can be symbolicated later.

set -eu

[ "$#" -eq 3 ] || {
    echo "usage: release-symbols.sh <target> <asset> <out-dir>" >&2
    exit 2
}
target=$1
asset=$2
out=$3
built="target/$target/release"
mkdir -p "$out"

case "$target" in
    *-linux-*)
        cp "$built/amux" "$out/$asset"
        objcopy --only-keep-debug "$out/$asset" "$out/$asset.debug"
        # The debuglink records the symbols file's name and checksum.
        (cd "$out" && objcopy --strip-all --add-gnu-debuglink="$asset.debug" "$asset")
        ;;
    *-apple-darwin)
        # The dSYM was written at link time (-Csplit-debuginfo=packed,
        # scripts/release-build.py), before anything was stripped.
        [ -d "$built/amux.dSYM" ] || {
            echo "release-symbols: $built/amux.dSYM is missing" >&2
            exit 1
        }
        cp "$built/amux" "$out/$asset"
        strip "$out/$asset"
        # Stripping invalidates the linker's ad hoc signature, and an arm64
        # Mac will not run an unsigned binary.
        codesign --force --sign - "$out/$asset"
        # Copy the bundle's contents so the archive holds it under the
        # asset's name.
        cp -R "$built/amux.dSYM/" "$out/$asset.dSYM"
        (cd "$out" && ditto -c -k --norsrc --noextattr --keepParent "$asset.dSYM" "$asset.dSYM.zip" && rm -r "$asset.dSYM")
        ;;
    *-windows-msvc)
        # The linker already keeps symbols in the PDB, not the executable.
        cp "$built/amux.exe" "$out/$asset"
        cp "$built/amux.pdb" "$out/${asset%.exe}.pdb"
        # The login item's launcher, shipped beside it:
        # amux-launch-windows-x86_64.exe for amux-windows-x86_64.exe.
        launcher="amux-launch-${asset#amux-}"
        cp "$built/amux-launch.exe" "$out/$launcher"
        cp "$built/amux-launch.pdb" "$out/${launcher%.exe}.pdb"
        ;;
    *)
        echo "release-symbols: no symbol split for $target" >&2
        exit 1
        ;;
esac

ls -l "$out"
