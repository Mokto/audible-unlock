#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=0.13.0
OUT="target/uv-$VERSION"
[[ -x "$OUT/uv" ]] && exit 0

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
for ARCH in aarch64 x86_64; do
  case "$ARCH" in
    aarch64) SHA256=a9c1b29002cf3c83f07fa9cd8a887a3be0107d90e23189721221e7257db8e3d6 ;;
    x86_64) SHA256=5f44dcbde809b632f47c36fadb241cb4d6f9af71d0c8f172b5d2026d3dde742c ;;
  esac
  NAME="uv-$ARCH-apple-darwin"
  curl -fsSL "https://github.com/astral-sh/uv/releases/download/$VERSION/$NAME.tar.gz" -o "$WORK/$NAME.tar.gz"
  echo "$SHA256  $WORK/$NAME.tar.gz" | shasum -a 256 -c - >/dev/null
  tar -xzf "$WORK/$NAME.tar.gz" -C "$WORK"
done
mkdir -p "$WORK/out"
lipo -create "$WORK/uv-aarch64-apple-darwin/uv" "$WORK/uv-x86_64-apple-darwin/uv" -output "$WORK/out/uv"
curl -fsSL "https://raw.githubusercontent.com/astral-sh/uv/$VERSION/LICENSE-MIT" -o "$WORK/out/LICENSE-MIT.txt"
printf 'uv %s by Astral, MIT OR Apache-2.0, from\nhttps://github.com/astral-sh/uv/releases/tag/%s (checksum-pinned in scripts/fetch-uv.sh).\n' \
  "$VERSION" "$VERSION" >"$WORK/out/README.txt"
rm -rf "$OUT"
mv "$WORK/out" "$OUT"
echo "Fetched $OUT"
