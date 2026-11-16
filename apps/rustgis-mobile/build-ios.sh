#!/usr/bin/env bash
# Builds the iOS app (#1902) on macOS with Xcode's command-line tools: target/ios/RustGIS.app.
#
#   apps/rustgis-mobile/build-ios.sh [--debug] [simulator|device]     (default: simulator)
#   apps/rustgis-mobile/build-ios.sh --sign-only
#
# simulator: for the iOS Simulator on an Apple-silicon Mac (aarch64-apple-ios-sim), signed ad hoc.
# device:    for iPhones and iPads (aarch64-apple-ios), signed with IOS_SIGN_IDENTITY and the
#            provisioning profile IOS_PROFILE (a .mobileprovision for org.rustgis.mobile), and
#            packed as target/ios/RustGIS.ipa for TestFlight. Without them the app is built
#            unsigned, with no .ipa, and the script exits 1; IOS_UNSIGNED=1 asks for that build
#            and exits 0. --sign-only then signs that bundle and packs the .ipa, building
#            nothing: CI builds with no signing identity on the machine (every crate's build
#            script runs during the build) and signs in a step of its own.
#
# Needs: Xcode (xcrun, codesign, plutil) and the Rust iOS targets (see README.md). There is no
# Xcode project: cargo builds the executable, and this script makes the bundle around it.
set -euo pipefail

profile=release
kind=simulator
sign_only=
for arg in "$@"; do
    case $arg in
        --debug) profile=debug ;;
        --sign-only) sign_only=1; kind=device ;;
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

# Signs the device bundle with IOS_SIGN_IDENTITY and the profile IOS_PROFILE, and packs the .ipa.
sign_app() {
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
}
if [ -n "$sign_only" ]; then
    [ "$kind" = device ] || { echo "build-ios: --sign-only signs a device build, not a $kind one" >&2; exit 1; }
    [ -x "$app/RustGIS" ] || { echo "build-ios: no app to sign at $app: build it with IOS_UNSIGNED=1 first" >&2; exit 1; }
    sign_app
    exit 0
fi

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
# RustGIS's licence and the notices of the crates it is built from go with every copy.
cp "$root/LICENSE" "$root/apps/rustgis-mobile/THIRD-PARTY-LICENSES.txt" "$app/"

version=$(awk '/^\[/ { s = ($0 == "[workspace.package]") } s && /^version *=/ { gsub(/.*= *"|".*/, ""); print; exit }' "$root/Cargo.toml")
[ -n "$version" ] || { echo "build-ios: no version in $root/Cargo.toml" >&2; exit 1; }
# The build number App Store Connect wants rising with each upload: IOS_BUILD, or the commits.
# A shallow clone or a tree without git can't count them: for a device (an upload) that is
# refused, as a repeated number would be; the Simulator's build takes 1.
build=${IOS_BUILD:-}
if [ -z "$build" ]; then
    if [ "$(git -C "$root" rev-parse --is-shallow-repository 2>/dev/null)" = false ]; then
        build=$(git -C "$root" rev-list --count HEAD)
    elif [ "$kind" = device ]; then
        echo "build-ios: no full git history to number the build: set IOS_BUILD" >&2
        exit 1
    else
        build=1
    fi
fi
sed -e "s/@VERSION@/$version/" -e "s/@BUILD@/$build/" -e "s/@PLATFORM@/$platform/" "$here/ios/Info.plist" > "$app/Info.plist"
# What Xcode's own builds record, which App Store Connect checks an upload for.
sdk_version=$(xcrun --sdk "$sdk" --show-sdk-version)
sdk_build=$(xcrun --sdk "$sdk" --show-sdk-build-version)
xcode=$(xcodebuild -version | sed -n 's/^Xcode \([0-9]*\)\.\([0-9]*\).*/\1\2/p' | head -1)
xcode_build=$(xcodebuild -version | sed -n 's/^Build version //p' | head -1)
platform_lower=$(echo "$sdk" | tr '[:upper:]' '[:lower:]')
for kv in "DTPlatformName string $platform_lower" "DTPlatformVersion string $sdk_version" \
    "DTSDKName string $platform_lower$sdk_version" "DTSDKBuild string $sdk_build" \
    "DTPlatformBuild string $sdk_build" "DTXcode string ${xcode}0" "DTXcodeBuild string $xcode_build" \
    "DTCompiler string com.apple.compilers.llvm.clang.1_0" "BuildMachineOSBuild string $(sw_vers -buildVersion)"; do
    set -- $kv
    /usr/libexec/PlistBuddy -c "Add :$1 $2 $3" "$app/Info.plist" >/dev/null
done
plutil -lint -s "$app/Info.plist"
printf 'APPL????' > "$app/PkgInfo"

# The icon, which build.rs drew from the logo's code: an asset catalog of its 1024-pixel images
# (the icon, and the dark and tinted ones iOS 18 and later show when asked), compiled by actool
# into the bundle, with the keys it adds merged into Info.plist.
# (Cargo keeps them in build/rustgis-mobile-<hash>/out, or build/rustgis-mobile/<hash>/out in newer
# versions: the newest of either.)
icon=$(find "$target_dir/$target/$profile/build" -path '*rustgis-mobile*' -name AppIcon.png -exec ls -t {} + 2>/dev/null | head -1) || true
[ -n "$icon" ] || { echo "build-ios: build.rs drew no AppIcon.png" >&2; exit 1; }
assets=$out/Assets.xcassets
rm -rf "$assets" && mkdir -p "$assets/AppIcon.appiconset"
for name in AppIcon AppIcon-Dark AppIcon-Tinted; do
    [ -f "$(dirname "$icon")/$name.png" ] || { echo "build-ios: build.rs drew no $name.png" >&2; exit 1; }
    cp "$(dirname "$icon")/$name.png" "$assets/AppIcon.appiconset/"
done
cat > "$assets/Contents.json" <<'JSON'
{ "info": { "author": "xcode", "version": 1 } }
JSON
cat > "$assets/AppIcon.appiconset/Contents.json" <<'JSON'
{
  "images": [
    { "filename": "AppIcon.png", "idiom": "universal", "platform": "ios", "size": "1024x1024" },
    { "appearances": [{ "appearance": "luminosity", "value": "dark" }],
      "filename": "AppIcon-Dark.png", "idiom": "universal", "platform": "ios", "size": "1024x1024" },
    { "appearances": [{ "appearance": "luminosity", "value": "tinted" }],
      "filename": "AppIcon-Tinted.png", "idiom": "universal", "platform": "ios", "size": "1024x1024" }
  ],
  "info": { "author": "xcode", "version": 1 }
}
JSON
xcrun actool "$assets" --compile "$app" --platform "$sdk" --minimum-deployment-target 15.0 \
    --target-device iphone --target-device ipad --app-icon AppIcon \
    --output-partial-info-plist "$out/icon.plist" --output-format human-readable-text --notices --warnings >/dev/null
/usr/libexec/PlistBuddy -c "Merge $out/icon.plist" "$app/Info.plist" >/dev/null

if [ "$kind" = simulator ]; then
    # The simulator runs anything signed, even ad hoc.
    codesign --force --sign - --timestamp=none "$app"
    echo "$app"
    exit 0
fi

if [ "${IOS_UNSIGNED:-}" = 1 ]; then
    # Asked for (CI's pull request builds): the bundle is complete, and isn't signed.
    echo "build-ios: $app is complete and unsigned (IOS_UNSIGNED=1); no .ipa was made."
    exit 0
fi
sign_app
