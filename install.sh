#!/bin/sh
# Install rsc from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/sbharga/rsc/master/install.sh | sh
#
# On Linux this installs the static musl build (any distro), falling back to the glibc
# build for releases that predate it. The SHA-256 is always checked; the Sigstore
# signature is checked too when cosign is on PATH.
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

arch_name=$(uname -m)
case $arch_name in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) err "unsupported architecture: $arch_name" ;;
esac
os_name=$(uname -s)
case $os_name in
    # Static musl build first; releases before 0.3.0 only have glibc builds.
    Linux) targets="$arch-unknown-linux-musl $arch-unknown-linux-gnu" ;;
    Darwin) targets=$arch-apple-darwin ;;
    *) err "unsupported OS: $os_name (on Windows use install.ps1)" ;;
esac

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

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading rsc $tag..."
target=
for t in $targets; do
    asset=rsc-$t.tar.gz
    url=https://github.com/sbharga/rsc/releases/download/$tag/$asset
    if curl -fsSL "$url" -o "$tmp/$asset" 2>/dev/null; then
        target=$t
        break
    fi
done
[ -n "$target" ] || err "download failed: $url"
curl -fsSL "$url.sha256" -o "$tmp/$asset.sha256" || err "checksum download failed"

want=$(cut -d ' ' -f 1 "$tmp/$asset.sha256")
if command -v sha256sum >/dev/null 2>&1; then
    got=$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)
else
    got=$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)
fi
[ "$want" = "$got" ] || err "checksum mismatch"

if command -v cosign >/dev/null 2>&1; then
    if curl -fsSL "$url.sigstore.json" -o "$tmp/$asset.sigstore.json" 2>/dev/null; then
        cosign verify-blob "$tmp/$asset" --bundle "$tmp/$asset.sigstore.json" \
            --certificate-identity "https://github.com/sbharga/rsc/.github/workflows/release.yml@refs/tags/$tag" \
            --certificate-oidc-issuer https://token.actions.githubusercontent.com >/dev/null 2>"$tmp/cosign.log" ||
            { cat "$tmp/cosign.log" >&2; err "signature verification failed"; }
        echo "Verified signature with cosign"
    else
        echo "No signature for $tag; verified SHA-256 only"
    fi
fi

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$dir"
cp "$tmp/rsc-$target/rsc" "$dir/rsc.tmp"
chmod 755 "$dir/rsc.tmp"
mv -f "$dir/rsc.tmp" "$dir/rsc"

echo "Installed rsc $tag ($target) to $dir/rsc"
case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "Add $dir to your PATH, e.g. export PATH=\"$dir:\$PATH\"" ;;
esac
