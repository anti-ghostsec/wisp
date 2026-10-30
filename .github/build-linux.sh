#!/bin/sh
# Builds Wisp for Linux (x86-64) and packs it for FerroOS's updater, which
# installs the newest of these builds (github.com/anti-ghostsec/FerroOS).
#
#   .github/build-linux.sh OUT
#
# Run as root on Debian trixie to get the build tools too (the release
# workflow runs it in a debian:trixie container, so Wisp runs on that
# release's WebKitGTK and GTK); run as anyone else, it only builds and packs.
#
# OUT gets wisp-linux-x86_64.tar.gz (wisp/wisp, wisp/plugins/, wisp/commit),
# its .sha256, and wisp-commit: the commit built, for checking without
# downloading the rest.

set -eu
umask 022

out=${1:?usage: build-linux.sh OUT}
here=$(cd "$(dirname "$0")/.." && pwd)
commit=${WISP_COMMIT:-$(git -C "$here" rev-parse HEAD)}
case "$commit" in
    *[!0-9a-f]* | "") echo "build-linux: $commit isn't a commit hash" >&2; exit 1 ;;
esac

if [ "$(id -u)" = 0 ]; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -q
    apt-get install -y -q --no-install-recommends ca-certificates curl build-essential pkg-config \
        libwebkitgtk-6.0-dev libgtk-4-dev >/dev/null
fi
# Debian's Rust can be older than Wisp's libraries need.
if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain stable
    . "$HOME/.cargo/env"
fi
cargo build --release --locked --manifest-path "$here/Cargo.toml"

pack=$(mktemp -d)
mkdir "$pack/wisp"
install -m 755 "$here/target/release/wisp" "$pack/wisp/wisp"
cp -r "$here/plugins" "$pack/wisp/plugins"
find "$pack/wisp/plugins" -name __pycache__ -prune -exec rm -rf {} +
chmod -R u=rwX,go=rX "$pack/wisp"
echo "$commit" >"$pack/wisp/commit"
mkdir -p "$out"
tar -czf "$out/wisp-linux-x86_64.tar.gz" -C "$pack" --owner=0 --group=0 --numeric-owner wisp
(cd "$out" && sha256sum wisp-linux-x86_64.tar.gz >wisp-linux-x86_64.tar.gz.sha256)
echo "$commit" >"$out/wisp-commit"
rm -rf "$pack"
echo "build-linux: Wisp $commit in $out"
