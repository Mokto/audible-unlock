#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="${1:-debug}"
case "$PROFILE" in
  debug) cargo build --locked ;;
  release) cargo build --locked --release ;;
  *) echo "Usage: $0 [debug|release]" >&2; exit 1 ;;
esac

APP="dist/Audible Unlock.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/backend/backend"
HELPER="$("target/$PROFILE/audible-unlock" --print-login-helper)"
cp "target/$PROFILE/audible-unlock" "$APP/Contents/MacOS/"
cp "$HELPER" "$APP/Contents/MacOS/audible-login"
cp pyproject.toml uv.lock "$APP/Contents/Resources/backend/"
cp backend/worker.py "$APP/Contents/Resources/backend/backend/"
cp resources/Info.plist "$APP/Contents/"
cp resources/AppIcon.icns "$APP/Contents/Resources/"
codesign --force --sign - "$APP/Contents/MacOS/audible-login"
codesign --force --sign - "$APP"
echo "Built $PWD/$APP"
