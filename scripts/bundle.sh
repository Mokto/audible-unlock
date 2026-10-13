#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="${1:-debug}"
case "$PROFILE" in
  debug) cargo build --locked ;;
  release) cargo build --locked --release ;;
  *) echo "Usage: $0 [debug|release]" >&2; exit 1 ;;
esac
bash scripts/build-ffmpeg.sh
FFMPEG=target/ffmpeg-9.0.2

APP="dist/Earshelf.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/backend/backend"
HELPER="$("target/$PROFILE/earshelf" --print-login-helper)"
cp "target/$PROFILE/earshelf" "$APP/Contents/MacOS/"
cp "$HELPER" "$APP/Contents/MacOS/earshelf-login"
cp pyproject.toml uv.lock "$APP/Contents/Resources/backend/"
cp backend/worker.py "$APP/Contents/Resources/backend/backend/"
cp resources/Info.plist "$APP/Contents/"
cp resources/AppIcon.icns "$APP/Contents/Resources/"
cp "$FFMPEG/bin/ffmpeg" "$FFMPEG/bin/ffprobe" "$APP/Contents/MacOS/"
mkdir -p "$APP/Contents/Resources/ffmpeg"
cp "$FFMPEG/LICENSE.txt" "$FFMPEG/README.txt" "$APP/Contents/Resources/ffmpeg/"
for tool in earshelf-login ffmpeg ffprobe; do codesign --force --sign - "$APP/Contents/MacOS/$tool"; done
codesign --force --sign - "$APP"
echo "Built $PWD/$APP"
