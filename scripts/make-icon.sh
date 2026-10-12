#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

TEMP=$(mktemp -d)
trap 'rm -rf "$TEMP"' EXIT
ICONSET="$TEMP/AppIcon.iconset"
mkdir -p "$ICONSET"
swift scripts/draw-icon.swift "$TEMP/master.png"
sips -z 1024 1024 "$TEMP/master.png" --out resources/AppIcon.png >/dev/null
for SIZE in 16 32 128 256 512; do
  sips -z "$SIZE" "$SIZE" resources/AppIcon.png --out "$ICONSET/icon_${SIZE}x${SIZE}.png" >/dev/null
  RETINA=$((SIZE * 2))
  sips -z "$RETINA" "$RETINA" resources/AppIcon.png --out "$ICONSET/icon_${SIZE}x${SIZE}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o resources/AppIcon.icns
iconutil -c iconset resources/AppIcon.icns -o "$TEMP/Verified.iconset"
for SIZE in 16 32 128 256 512; do
  test -s "$TEMP/Verified.iconset/icon_${SIZE}x${SIZE}.png"
  test -s "$TEMP/Verified.iconset/icon_${SIZE}x${SIZE}@2x.png"
done
sips -g pixelWidth -g pixelHeight resources/AppIcon.png
