#!/usr/bin/env bash
# Builds the iOS app (#1902) on macOS with Xcode's command-line tools: target/ios/RustGIS.app.
#
#   apps/rustgis-mobile/build-ios.sh [--debug] [simulator|device]     (default: simulator)
#
# simulator: for the iOS Simulator on an Apple-silicon Mac (aarch64-apple-ios-sim), signed ad hoc.
# device:    for iPhones and iPads (aarch64-apple-ios), signed with IOS_SIGN_IDENTITY and the
#            provisioning profile IOS_PROFILE (a .mobileprovision for org.rustgis.mobile), and
#            packed as target/ios/RustGIS.ipa for TestFlight. Without them the app is built
#            unsigned, with no .ipa, and the script exits 1.
#
# Needs: Xcode (xcrun, codesign, plutil) and the Rust iOS targets (see README.md). There is no
# Xcode project: cargo builds the executable, and this script makes the bundle around it.
set -euo pipefail

profile=release
kind=simulator
for arg in "$@"; do
    case $arg in
        --debug) profile=debug ;;
        simulator | device) kind=$arg ;;
        *) echo "build-ios: say simulator or device, not $arg (and --debug, if wanted)" >&2; exit 1 ;;
    esac
done
case $kind in
    simulator) target=aarch64-apple-ios-sim; platform=iPhoneSimulator; sdk=iphonesimulator ;;
    device) target=aarch64-apple-ios; platform=iPhoneOS; sdk=iphoneos ;;
esac

root=$(cd "$(dirname "$0")/../.." && pwd)
here=$root/apps/rustgis-mobile
# Cargo runs in the checkout's root, where a relative CARGO_TARGET_DIR is read from.
target_dir=${CARGO_TARGET_DIR:-target}
case $target_dir in /*) ;; *) target_dir=$root/$target_dir ;; esac
out=$target_dir/ios
app=$out/RustGIS.app

# macOS's bash 3.2 can't expand an empty array under `set -u`: a flag that may be empty instead.
flag=
if [ "$profile" = release ]; then flag=--release; fi
# The SDK's own clang builds the C parts (the bundled SQLite, the Zstandard decoder).
SDKROOT=$(xcrun --sdk "$sdk" --show-sdk-path)
export SDKROOT
export IPHONEOS_DEPLOYMENT_TARGET=15.0
(cd "$root" && cargo build $flag --target "$target" -p rustgis-mobile --bin rustgis-mobile)

rm -rf "$app" && mkdir -p "$app"
cp "$target_dir/$target/$profile/rustgis-mobile" "$app/RustGIS"
# Without its symbols: they are most of the executable's size.
if [ "$profile" = release ]; then xcrun strip -x "$app/RustGIS"; fi

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
# The build number App Store Connect wants rising with each upload: IOS_BUILD, or the commits.
build=${IOS_BUILD:-$(git -C "$root" rev-list --count HEAD 2>/dev/null || echo 1)}
sed -e "s/@VERSION@/$version/" -e "s/@BUILD@/$build/" -e "s/@PLATFORM@/$platform/" "$here/ios/Info.plist" > "$app/Info.plist"
plutil -lint -s "$app/Info.plist"
printf 'APPL????' > "$app/PkgInfo"

if [ "$kind" = simulator ]; then
    # The simulator runs anything signed, even ad hoc.
    codesign --force --sign - --timestamp=none "$app"
    echo "$app"
    exit 0
fi

if [ -z "${IOS_SIGN_IDENTITY:-}" ] || [ -z "${IOS_PROFILE:-}" ]; then
    # Not a success: there is no .ipa, and the app won't install on a device.
    echo "build-ios: IOS_SIGN_IDENTITY and IOS_PROFILE aren't both set: $app is unsigned, and no .ipa was made." >&2
    exit 1
fi
cp "$IOS_PROFILE" "$app/embedded.mobileprovision"
# The entitlements are the profile's own (the team, the app id, background location needs none).
security cms -D -i "$IOS_PROFILE" > "$out/profile.plist"
plutil -extract Entitlements xml1 -o "$out/entitlements.plist" "$out/profile.plist"
codesign --force --sign "$IOS_SIGN_IDENTITY" --entitlements "$out/entitlements.plist" --timestamp=none "$app"
rm -rf "$out/Payload" "$out/RustGIS.ipa" && mkdir -p "$out/Payload"
cp -R "$app" "$out/Payload/"
(cd "$out" && zip -qry RustGIS.ipa Payload)
rm -rf "$out/Payload"
ls -lh "$out/RustGIS.ipa"
