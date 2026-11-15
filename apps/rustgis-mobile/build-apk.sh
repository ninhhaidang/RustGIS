#!/usr/bin/env bash
# Builds a release APK of the Android app (#1902): target/android/rustgis-mobile.apk. --debug
# builds the debug profile instead.
#
#   apps/rustgis-mobile/build-apk.sh [--debug] [abi ...]     (default: arm64-v8a x86_64)
#   apps/rustgis-mobile/build-apk.sh --sign-only
#
# Signed with a debug key of this checkout's own, or, with ANDROID_KEYSTORE (a keystore file) and
# ANDROID_KEYSTORE_PASSWORD, with the release key in it (alias ANDROID_KEY_ALIAS, default
# rustgis), as the APKs on GitHub releases are. Its version is the workspace's; its version code,
# which Android wants rising with each update, is ANDROID_VERSION_CODE or the count of commits.
#
# ANDROID_UNSIGNED=1 leaves the APK unsigned (target/android/aligned.apk), and --sign-only then
# signs it with the release key and builds nothing: CI builds with no secret in the build's
# environment (every crate's build script runs in it) and signs in a step of its own.
#
# Needs: a JDK (keytool, apksigner), the Rust Android targets, cargo-ndk, and an Android SDK with
# the NDK, build-tools and a platform (see README.md). ANDROID_HOME names the SDK (default ~/Android/Sdk).
set -euo pipefail

profile=--release
sign_only=
case ${1:-} in
    --debug) profile=; shift ;;
    --sign-only) sign_only=1; shift ;;
esac
abis=("$@")
[ ${#abis[@]} -gt 0 ] || abis=(arm64-v8a x86_64)

root=$(cd "$(dirname "$0")/../.." && pwd)
sdk=${ANDROID_HOME:-$HOME/Android/Sdk}
# The newest of each that is installed.
# Nothing when there is none: the check below says what is missing.
newest() { { ls -d "$1"/*/ 2>/dev/null || true; } | sort -V | tail -1 | sed 's:/$::'; }
export ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-$(newest "$sdk/ndk")}
tools=$(newest "$sdk/build-tools")
platform=$(newest "$sdk/platforms")
# Signing needs only the build-tools (apksigner).
needs=("$tools")
[ -n "$sign_only" ] || needs+=("$ANDROID_NDK_HOME" "$platform")
for need in "${needs[@]}"; do
    [ -d "$need" ] || { echo "build-apk: the Android SDK at $sdk has no NDK, build-tools or platform (see README.md)" >&2; exit 1; }
done
out=$root/target/android

# Signs the unsigned APK with the release key; its password is read from the environment, never
# put on a command line.
sign_release() {
    : "${ANDROID_KEYSTORE:?build-apk: ANDROID_KEYSTORE names no keystore}"
    : "${ANDROID_KEYSTORE_PASSWORD:?build-apk: ANDROID_KEYSTORE is set but ANDROID_KEYSTORE_PASSWORD is not}"
    [ -f "$out/aligned.apk" ] || { echo "build-apk: no unsigned APK to sign: build with ANDROID_UNSIGNED=1 first" >&2; exit 1; }
    "$tools/apksigner" sign --ks "$ANDROID_KEYSTORE" --ks-key-alias "${ANDROID_KEY_ALIAS:-rustgis}" \
        --ks-pass env:ANDROID_KEYSTORE_PASSWORD --out "$out/rustgis-mobile.apk" "$out/aligned.apk"
    rm -f "$out/aligned.apk"
    ls -lh "$out/rustgis-mobile.apk"
}
if [ -n "$sign_only" ]; then
    sign_release
    exit 0
fi

rm -rf "$out/apk" "$out/rustgis-mobile.apk" && mkdir -p "$out/apk"
targets=()
for abi in "${abis[@]}"; do targets+=(-t "$abi"); done
# API 26 (Android 8.0): the oldest the app is linked for.
(cd "$root" && cargo ndk "${targets[@]}" -P 26 -o "$out/apk/lib" build $profile -p rustgis-mobile)
# Only the app's library goes in (cargo-ndk also copies a dependency's own cdylib), without its
# symbols: 115 MB with them, under 80 MB without.
find "$out/apk/lib" -name '*.so' ! -name librustgis_mobile.so -delete
strip=$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-strip | head -1)
find "$out/apk/lib" -name librustgis_mobile.so -exec "$strip" {} \;

# The activity's Java half, compiled without Gradle: javac, then d8 to Android's bytecode.
rm -rf "$out/java" && mkdir -p "$out/java/classes" "$out/java/dex"
mapfile -d '' sources < <(find "$root/apps/rustgis-mobile/java" -name '*.java' -print0)
javac -nowarn -source 11 -target 11 -classpath "$platform/android.jar" -d "$out/java/classes" "${sources[@]}"
mapfile -d '' classes < <(find "$out/java/classes" -name '*.class' -print0)
"$tools/d8" --release --min-api 26 --lib "$platform/android.jar" --output "$out/java/dex" "${classes[@]}"

version=$(awk '/^\[/ { s = ($0 == "[workspace.package]") } s && /^version *=/ { gsub(/.*= *"|".*/, ""); print; exit }' "$root/Cargo.toml")
[ -n "$version" ] || { echo "build-apk: no version in $root/Cargo.toml" >&2; exit 1; }
code=${ANDROID_VERSION_CODE:-}
if [ -z "$code" ]; then
    if [ "$(git -C "$root" rev-parse --is-shallow-repository 2>/dev/null)" = false ]; then
        code=$(git -C "$root" rev-list --count HEAD)
    elif [ -n "${ANDROID_KEYSTORE:-}" ] || [ "${ANDROID_UNSIGNED:-}" = 1 ]; then
        # A release build (signed now, or by --sign-only later) numbered 1 would never update an
        # installed one.
        echo "build-apk: no full git history to number the release: set ANDROID_VERSION_CODE" >&2
        exit 1
    else
        code=1
    fi
fi

unsigned=$out/unsigned.apk
"$tools/aapt2" link -o "$unsigned" --manifest "$root/apps/rustgis-mobile/AndroidManifest.xml" -I "$platform/android.jar" \
    --version-code "$code" --version-name "$version" --replace-version
# The libraries go in uncompressed and page-aligned, so Android maps them straight from the APK.
(cd "$out/apk" && zip -q -0 -r "$unsigned" lib)
(cd "$out/java/dex" && zip -q "$unsigned" classes.dex)
# RustGIS's licence and the notices of the crates it is built from go with every copy.
rm -rf "$out/notices" && mkdir -p "$out/notices/assets"
cp "$root/LICENSE" "$root/apps/rustgis-mobile/THIRD-PARTY-LICENSES.txt" "$out/notices/assets/"
(cd "$out/notices" && zip -q "$unsigned" assets/LICENSE assets/THIRD-PARTY-LICENSES.txt)
# -P 16: on 16 KB pages, which Android 15 phones may use (build-tools 35 and later).
"$tools/zipalign" -f -P 16 4 "$unsigned" "$out/aligned.apk"
rm -f "$unsigned"

if [ "${ANDROID_UNSIGNED:-}" = 1 ]; then
    echo "build-apk: $out/aligned.apk is unsigned (ANDROID_UNSIGNED=1): sign it with --sign-only"
elif [ -n "${ANDROID_KEYSTORE:-}" ]; then
    sign_release
else
    # A debug key of this checkout's own: an APK must be signed to install, even on an emulator.
    key=$out/debug.keystore
    [ -f "$key" ] || keytool -genkeypair -keystore "$key" -storepass android -keypass android -alias debug \
        -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=RustGIS debug" >/dev/null
    "$tools/apksigner" sign --ks "$key" --ks-pass pass:android --key-pass pass:android --out "$out/rustgis-mobile.apk" "$out/aligned.apk"
    rm -f "$out/aligned.apk"
    ls -lh "$out/rustgis-mobile.apk"
fi
