#!/bin/sh
# Builds Wisp for Android:
#
#   android/build-android.sh [ABI] [debug|release]
#
# ABI is arm64-v8a (phones, the default) or x86_64 (the emulator). Needs the
# Android SDK and NDK (ANDROID_HOME, default ~/.local/opt/android-sdk),
# cargo-ndk, the Rust target for the ABI, and a JDK (JAVA_HOME).
#
# 1. the Rust core (wisp-core through wisp-ffi) for that ABI, and its Kotlin
#    bindings (UniFFI);
# 2. Wisp's plugins as built-in GeckoView extensions, with EasyList,
#    EasyPrivacy and uBlock's badware list as blocking rules (downloaded,
#    kept for 4 days in android/build/lists);
# 3. the app (Gradle), in android/app/build/outputs/apk/.

set -eu
here=$(cd "$(dirname "$0")" && pwd)
abi=${1:-arm64-v8a}
kind=${2:-debug}
case "$abi" in
    arm64-v8a) target=aarch64-linux-android ;;
    x86_64) target=x86_64-linux-android ;;
    *) echo "build-android: ABI is arm64-v8a or x86_64" >&2; exit 2 ;;
esac
case "$kind" in debug | release) ;; *) echo "build-android: debug or release" >&2; exit 2 ;; esac

: "${ANDROID_HOME:=$HOME/.local/opt/android-sdk}"
export ANDROID_HOME
ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -1)}
[ -d "$ANDROID_NDK_HOME" ] || { echo "build-android: no NDK in $ANDROID_HOME/ndk" >&2; exit 1; }
export ANDROID_NDK_HOME
# No paths of the computer it's built on end up in the library.
export RUSTFLAGS="--remap-path-prefix=$HOME=~"

rust=$here/rust
app=$here/app/src/main
echo "build-android: Rust core for $abi"
cargo ndk --manifest-path "$rust/Cargo.toml" -t "$abi" --platform 26 -o "$app/jniLibs" build --release --locked --lib
cargo build --manifest-path "$rust/Cargo.toml" --release --locked --bins
"$rust/target/release/uniffi-bindgen" generate --library "$app/jniLibs/$abi/libwisp_ffi.so" \
    --language kotlin --out-dir "$app/java" --no-format >/dev/null

echo "build-android: plugins"
lists=$here/build/lists
mkdir -p "$lists"
fetch() {
    # name url
    file=$lists/$1.txt
    if [ ! -s "$file" ] || [ -n "$(find "$file" -mtime +4)" ]; then
        curl --proto '=https' --tlsv1.2 -fsSL --max-filesize 33554432 -o "$file.tmp" "$2" && mv "$file.tmp" "$file" ||
            { rm -f "$file.tmp"; [ -s "$file" ] || { echo "build-android: cannot download $2" >&2; exit 1; }; }
    fi
}
fetch easylist https://easylist.to/easylist/easylist.txt
fetch easyprivacy https://easylist.to/easylist/easyprivacy.txt
fetch badware https://ublockorigin.github.io/uAssets/filters/badware.txt
"$rust/target/release/wisp-extensions" "$here/../plugins" "$app/assets/extensions" \
    --list "adblock=$lists/easylist.txt" --list "adblock=$lists/easyprivacy.txt" --list "adblock=$lists/badware.txt"

echo "build-android: app"
task=assemble$(printf %s "$kind" | cut -c1 | tr a-z A-Z)$(printf %s "$kind" | cut -c2-)
# WISP_VERSION_CODE / WISP_VERSION_NAME: what the release workflow numbers it.
"$here/gradlew" -p "$here" --no-daemon -q "$task" -Pabi="$abi" \
    ${WISP_VERSION_CODE:+-PversionCode="$WISP_VERSION_CODE"} ${WISP_VERSION_NAME:+-PversionName="$WISP_VERSION_NAME"}
ls "$here"/app/build/outputs/apk/"$kind"/*.apk
