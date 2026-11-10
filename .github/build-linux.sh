#!/bin/sh
# Builds Wisp for Linux (x86-64) and packs it for FerroOS's updater, which
# installs the newest of these builds (github.com/anti-ghostsec/FerroOS).
#
#   .github/build-linux.sh OUT
#
# With WISP_CHECK=1 it first runs clippy (warnings count as errors) and the
# tests, and builds nothing if either fails: the release workflow does that,
# so no release goes out that doesn't pass them.
#
# Run as root on Debian trixie to get the build tools too (the release
# workflow runs it in a debian:trixie container, so Wisp runs on that
# release's WebKitGTK and GTK); run as anyone else, it only builds and packs.
#
# OUT gets wisp-linux-x86_64.tar.gz (wisp/wisp, wisp/plugins/, wisp/commit),
# its .sha256, and wisp-commit: the commit built, for checking without
# downloading the rest. The workflow then signs the archive (minisign).

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
        libwebkitgtk-6.0-dev libgtk-4-dev python3 >/dev/null
fi
# Debian's Rust can be older than Wisp's libraries need. Rust's installer is
# fetched at a set version and run only if it's the file expected (its
# SHA-256, from static.rust-lang.org's rustup-init.sha256 for that version),
# and it installs a set Rust. To move to a newer one, change all three.
rustup_version=1.29.3
rustup_sha256=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55
rust_version=1.99.1
if ! command -v cargo >/dev/null 2>&1; then
    [ "$(uname -m)" = x86_64 ] || { echo "build-linux: no Rust, and its installer is only known here for x86_64" >&2; exit 1; }
    # In a folder of its own, under its own name: the installer goes by
    # the name it's run as, and takes any other for one of Rust's tools.
    tmp=$(mktemp -d)
    init=$tmp/rustup-init
    curl --proto '=https' --tlsv1.2 -fsSL -o "$init" \
        "https://static.rust-lang.org/rustup/archive/$rustup_version/x86_64-unknown-linux-gnu/rustup-init"
    echo "$rustup_sha256  $init" | sha256sum -c - >/dev/null || {
        echo "build-linux: rustup-init $rustup_version isn't the file expected" >&2
        rm -rf "$tmp"
        exit 1
    }
    chmod +x "$init"
    "$init" -y -q --profile minimal --default-toolchain "$rust_version" -c clippy
    rm -rf "$tmp"
    . "$HOME/.cargo/env"
fi
if [ "${WISP_CHECK:-}" = 1 ]; then
    cargo clippy --release --locked --all-targets --manifest-path "$here/Cargo.toml" -- -D warnings
    cargo test --release --locked --manifest-path "$here/Cargo.toml"
    python3 -I "$here/tests/vidgrab_host.py"
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
