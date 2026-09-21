#!/usr/bin/env bash
# Build the macOS installer: Apple's own package format, a .pkg that installs
# a universal Vagtplanlægning.app into /Applications. Needs a stable Rust
# toolchain and the Xcode command line tools.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version="$("$root/scripts/release-version.sh")"
# CFBundleShortVersionString is read as period-separated integers, so the
# dated release 2026.09.21 is written without its leading zeros.
IFS=. read -r year month day <<< "$version"
bundle_version="$year.$((10#$month)).$((10#$day))"

app="$root/target/macos/Vagtplanlægning.app"
pkg="$root/dist/teamup-shift-sync-$version-universal.pkg"

echo "Building Vagtplanlægning $version for macOS (universal)"

export MACOSX_DEPLOYMENT_TARGET=11.0
export TEAMUP_SHIFT_SYNC_VERSION="$version"
for target in x86_64-apple-darwin aarch64-apple-darwin; do
  rustup target add "$target"
  cargo build --release --locked --workspace --target "$target"
done

rm -rf "$root/target/macos"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/fixtures" "$root/dist"

for binary in teamup-shift-sync-gui teamup-shift-sync-rust; do
  lipo -create -output "$app/Contents/MacOS/$binary" \
    "target/x86_64-apple-darwin/release/$binary" \
    "target/aarch64-apple-darwin/release/$binary"
done

cp fixtures/offline-config.toml fixtures/representative-week.json \
  "$app/Contents/Resources/fixtures/"
sed "s/{{VERSION}}/$bundle_version/" packaging/macos/Info.plist > "$app/Contents/Info.plist"

iconset="$root/target/macos/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 64 128 256 512; do
  sips -z "$size" "$size" packaging/icons/teamup-shift-sync-1024.png \
    --out "$iconset/icon_${size}x${size}.png" >/dev/null
  sips -z "$((size * 2))" "$((size * 2))" packaging/icons/teamup-shift-sync-1024.png \
    --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil --convert icns "$iconset" --output "$app/Contents/Resources/AppIcon.icns"
rm -rf "$iconset"

echo "Verifying the built app bundle"
TEAMUP_SHIFT_SYNC_CONFIG="$app/Contents/Resources/fixtures/offline-config.toml" \
TEAMUP_FIXTURE="$app/Contents/Resources/fixtures/representative-week.json" \
  "$app/Contents/MacOS/teamup-shift-sync-gui" --self-check

rm -f "$pkg"
# The app is not signed or notarized yet, so macOS asks the person to confirm
# the first open. docs/installation.md documents the exact steps and what it
# would take to remove them.
productbuild --component "$app" /Applications "$pkg"

echo "macOS installer: $pkg"
