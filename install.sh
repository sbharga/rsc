#!/bin/sh
# Install rsc from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/sbharga/rsc/master/install.sh | sh
#
# Environment:
#   RSC_VERSION      release tag to install (default: newest release, pre-releases included)
#   RSC_INSTALL_DIR  install directory (default: $HOME/.local/bin)
set -eu

err() {
    echo "rsc install: $*" >&2
    exit 1
}

command -v curl >/dev/null 2>&1 || err "curl is required"
command -v tar >/dev/null 2>&1 || err "tar is required"

os_name=$(uname -s)
case $os_name in
    Linux) os=unknown-linux-gnu ;;
    Darwin) os=apple-darwin ;;
    *) err "unsupported OS: $os_name (Linux and macOS only)" ;;
esac
arch_name=$(uname -m)
case $arch_name in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) err "unsupported architecture: $arch_name" ;;
esac
target=$arch-$os

dir=${RSC_INSTALL_DIR:-$HOME/.local/bin}
tag=${RSC_VERSION:-}
if [ -z "$tag" ]; then
    tag=$(curl -fsSL "https://api.github.com/repos/sbharga/rsc/releases?per_page=1" |
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1) || true
fi
[ -n "$tag" ] || err "couldn't find a release"
case $tag in
    v*) ;;
    *) tag=v$tag ;;
esac

asset=rsc-$target.tar.gz
url=https://github.com/sbharga/rsc/releases/download/$tag/$asset
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading rsc $tag ($target)..."
curl -fsSL "$url" -o "$tmp/$asset" || err "download failed: $url"
curl -fsSL "$url.sha256" -o "$tmp/$asset.sha256" || err "checksum download failed"

want=$(cut -d ' ' -f 1 "$tmp/$asset.sha256")
if command -v sha256sum >/dev/null 2>&1; then
    got=$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)
else
    got=$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)
fi
[ "$want" = "$got" ] || err "checksum mismatch"

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$dir"
cp "$tmp/rsc-$target/rsc" "$dir/rsc.tmp"
chmod 755 "$dir/rsc.tmp"
mv -f "$dir/rsc.tmp" "$dir/rsc"

echo "Installed rsc $tag to $dir/rsc"
case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "Add $dir to your PATH, e.g. export PATH=\"$dir:\$PATH\"" ;;
esac
