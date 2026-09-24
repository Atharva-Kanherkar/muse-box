#!/usr/bin/env bash
# Builds build/muse-box.app from the Swift package.
#
#   ARCHS="arm64 x86_64"   universal binary (default: this Mac's arch)
#   VERSION=0.2.0          stamps CFBundleShortVersionString
#   SIGN_IDENTITY="Developer ID Application: …"
#                          hardened-runtime signing for notarization
#                          (default: ad-hoc, fine for your own Mac and for
#                          friends who right-click › Open)
set -euo pipefail
cd "$(dirname "$0")/.."

CONFIG="${CONFIG:-release}"
ARCHS="${ARCHS:-$(uname -m)}"
APP="build/muse-box.app"

binaries=()
for arch in $ARCHS; do
  triple="${arch}-apple-macosx14.0"
  swift build -c "$CONFIG" --triple "$triple" --product muse-box
  binaries+=("$(swift build -c "$CONFIG" --triple "$triple" --show-bin-path)/muse-box")
done

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create "${binaries[@]}" -output "$APP/Contents/MacOS/muse-box"
cp Resources/Info.plist "$APP/Contents/Info.plist"
cp Resources/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
cp -R Resources/Fonts "$APP/Contents/Resources/Fonts"

plist() { /usr/libexec/PlistBuddy -c "$1" "$APP/Contents/Info.plist"; }
if [ -n "${VERSION:-}" ]; then plist "Set :CFBundleShortVersionString ${VERSION#v}"; fi
if build=$(git rev-list --count HEAD 2>/dev/null); then plist "Set :CFBundleVersion $build"; fi

if [ -n "${SIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp \
    --entitlements Resources/MuseBox.entitlements --sign "$SIGN_IDENTITY" "$APP"
else
  codesign --force --sign - "$APP"
fi
codesign --verify --strict "$APP"
echo "$APP"
