#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# debug: host architecture only. release: universal (arm64 + x86_64) app plus a zip and checksum in dist/;
# needs rustup with the aarch64-apple-darwin and x86_64-apple-darwin targets.
PROFILE="${1:-debug}"
case "$PROFILE" in
  debug)
    cargo build --locked
    BINARY=target/debug/earshelf
    ;;
  release)
    for TARGET in aarch64-apple-darwin x86_64-apple-darwin; do
      MACOSX_DEPLOYMENT_TARGET=12.0 cargo build --locked --release --target "$TARGET"
    done
    BINARY=target/release/earshelf-universal
    lipo -create target/aarch64-apple-darwin/release/earshelf target/x86_64-apple-darwin/release/earshelf \
      -output "$BINARY"
    ;;
  *) echo "Usage: $0 [debug|release]" >&2; exit 1 ;;
esac
bash scripts/build-ffmpeg.sh
bash scripts/fetch-uv.sh
FFMPEG=target/ffmpeg-9.0.2-universal
UV=target/uv-0.13.0

APP="dist/Earshelf.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/backend/backend" \
  "$APP/Contents/Resources/ffmpeg" "$APP/Contents/Resources/uv"
if [[ "$PROFILE" == release ]]; then
  HELPER="$(target/aarch64-apple-darwin/release/earshelf --print-login-helper)"
else
  HELPER="$("$BINARY" --print-login-helper)"
fi
cp "$BINARY" "$APP/Contents/MacOS/earshelf"
cp "$HELPER" "$APP/Contents/MacOS/earshelf-login"
cp "$FFMPEG/bin/ffmpeg" "$FFMPEG/bin/ffprobe" "$UV/uv" "$APP/Contents/MacOS/"
cp "$FFMPEG/LICENSE.txt" "$FFMPEG/README.txt" "$APP/Contents/Resources/ffmpeg/"
cp "$UV/LICENSE-MIT.txt" "$UV/README.txt" "$APP/Contents/Resources/uv/"
cp pyproject.toml uv.lock "$APP/Contents/Resources/backend/"
cp backend/worker.py "$APP/Contents/Resources/backend/backend/"
cp resources/Info.plist "$APP/Contents/"
cp resources/AppIcon.icns "$APP/Contents/Resources/"
for TOOL in earshelf-login ffmpeg ffprobe uv; do codesign --force --sign - "$APP/Contents/MacOS/$TOOL"; done
codesign --force --sign - "$APP"
codesign --verify --deep --strict "$APP"
echo "Built $PWD/$APP"

if [[ "$PROFILE" == release ]]; then
  ZIP=dist/Earshelf-macos-universal.zip
  rm -f "$ZIP" "$ZIP.sha256"
  ditto -c -k --norsrc --noextattr --noacl --keepParent "$APP" "$ZIP"
  (cd dist && shasum -a 256 "$(basename "$ZIP")" >"$(basename "$ZIP").sha256")
  echo "Packaged $PWD/$ZIP"
fi
