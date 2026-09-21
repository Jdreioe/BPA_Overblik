#!/usr/bin/env bash
# Build the Linux installer: one self-contained AppImage with the desktop app,
# the CLI and the fixtures. Needs a stable Rust toolchain, curl and the build
# dependencies of the workspace (libdbus-1-dev).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version="$("$root/scripts/release-version.sh")"
app_id="io.github.jdreioe.teamup-shift-sync"
arch="x86_64"
out_dir="$root/dist"
appdir="$root/target/appimage/AppDir"
appimage="$out_dir/teamup-shift-sync-$version-$arch.AppImage"
appimagetool="$root/target/appimage/appimagetool-$arch.AppImage"
runtime="$root/target/appimage/runtime-$arch"

echo "Building Vagtplanlægning $version for Linux/$arch"

TEAMUP_SHIFT_SYNC_VERSION="$version" \
  cargo build --release --locked --workspace --target "$arch-unknown-linux-gnu"
binaries="$root/target/$arch-unknown-linux-gnu/release"

rm -rf "$appdir"
mkdir -p "$appdir/usr/bin" \
  "$appdir/usr/share/applications" \
  "$appdir/usr/share/metainfo" \
  "$appdir/usr/share/icons/hicolor/256x256/apps" \
  "$appdir/usr/share/teamup-shift-sync/fixtures" \
  "$out_dir"

install -m 755 "$binaries/teamup-shift-sync-gui" "$appdir/usr/bin/"
install -m 755 "$binaries/teamup-shift-sync-rust" "$appdir/usr/bin/"
install -m 644 fixtures/offline-config.toml fixtures/representative-week.json \
  "$appdir/usr/share/teamup-shift-sync/fixtures/"

install -m 644 "packaging/linux/$app_id.desktop" "$appdir/usr/share/applications/"
install -m 644 "packaging/linux/$app_id.desktop" "$appdir/$app_id.desktop"
install -m 644 packaging/icons/teamup-shift-sync-256.png \
  "$appdir/usr/share/icons/hicolor/256x256/apps/$app_id.png"
install -m 644 packaging/icons/teamup-shift-sync-256.png "$appdir/$app_id.png"
sed -e "s/{{VERSION}}/$version/" -e "s/{{DATE}}/${version//./-}/" \
  "packaging/linux/$app_id.appdata.xml" > "$appdir/usr/share/metainfo/$app_id.appdata.xml"

# The fixture paths only matter for --self-check, which is how a finished
# install is verified. Exporting them unconditionally would make the desktop
# app refuse to start, because fixture mode must never become a live workflow.
cat > "$appdir/AppRun" <<'APPRUN'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
resources="$here/usr/share/teamup-shift-sync"

if [ "${1:-}" = "cli" ]; then
  shift
  exec "$here/usr/bin/teamup-shift-sync-rust" "$@"
fi

if [ "${1:-}" = "--self-check" ]; then
  TEAMUP_SHIFT_SYNC_CONFIG="${TEAMUP_SHIFT_SYNC_CONFIG:-$resources/fixtures/offline-config.toml}" \
  TEAMUP_FIXTURE="${TEAMUP_FIXTURE:-$resources/fixtures/representative-week.json}" \
    exec "$here/usr/bin/teamup-shift-sync-gui" --self-check
fi

exec "$here/usr/bin/teamup-shift-sync-gui" "$@"
APPRUN
chmod 755 "$appdir/AppRun"

if [ ! -x "$appimagetool" ]; then
  echo "Downloading appimagetool"
  curl -fsSL \
    "https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-$arch.AppImage" \
    -o "$appimagetool"
  chmod +x "$appimagetool"
fi

if [ ! -s "$runtime" ]; then
  echo "Downloading the AppImage runtime"
  curl -fsSL \
    "https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-$arch" \
    -o "$runtime"
fi

rm -f "$appimage"
# FUSE is unavailable in containers, so run appimagetool extracted. The runtime
# is fetched above rather than by appimagetool, whose own download can hang.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$arch" \
  "$appimagetool" --runtime-file "$runtime" "$appdir" "$appimage"
chmod +x "$appimage"

echo "Verifying the packaged AppImage"
APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" --self-check

echo "Linux installer: $appimage"
