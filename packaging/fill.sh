#!/bin/sh
# Fill the Scoop, winget and AUR templates for a published GitHub release.
# Usage: packaging/fill.sh X.Y.Z [out-dir]   (default out-dir: target/packaging/X.Y.Z)
# Reads the checksums from the release's .sha256 files; changes nothing in packaging/.
set -eu

version=${1:?usage: packaging/fill.sh X.Y.Z [out-dir]}
version=${version#v}
here=$(cd "$(dirname "$0")" && pwd)
out=${2:-$here/../target/packaging/$version}
base="https://github.com/rajput-hemant/dman/releases/download/v$version"

sha() {
    sum=$(curl -fsSL "$base/dman-$1.sha256" | cut -d' ' -f1)
    case $sum in
        *[!0-9a-f]* | '') echo "bad checksum for dman-$1: '$sum'" >&2; exit 1 ;;
    esac
    [ ${#sum} -eq 64 ] || { echo "bad checksum for dman-$1: '$sum'" >&2; exit 1; }
    echo "$sum"
}

win=$(sha x86_64-pc-windows-msvc.zip)
linux_x86_64=$(sha x86_64-unknown-linux-musl.tar.gz)
linux_aarch64=$(sha aarch64-unknown-linux-musl.tar.gz)

fill() {
    mkdir -p "$(dirname "$2")"
    sed -e "s/__VERSION__/$version/g" \
        -e "s/__SHA256_WINDOWS_X86_64__/$win/g" \
        -e "s/__SHA256_LINUX_X86_64__/$linux_x86_64/g" \
        -e "s/__SHA256_LINUX_AARCH64__/$linux_aarch64/g" \
        "$1" > "$2"
}

fill "$here/scoop/dman.json" "$out/scoop/dman.json"
for f in "$here"/winget/*.yaml; do
    fill "$f" "$out/winget/$(basename "$f")"
done
fill "$here/aur/PKGBUILD" "$out/aur/PKGBUILD"
echo "filled templates for $version in $out"
