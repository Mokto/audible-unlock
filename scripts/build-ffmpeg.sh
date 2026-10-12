#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=9.0.2
SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
OUT="target/ffmpeg-$VERSION"
[[ -x "$OUT/bin/ffmpeg" && -x "$OUT/bin/ffprobe" ]] && exit 0

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
curl -fsSL "https://ffmpeg.org/releases/ffmpeg-$VERSION.tar.xz" -o "$WORK/src.tar.xz"
echo "$SHA256  $WORK/src.tar.xz" | shasum -a 256 -c - >/dev/null
tar -xf "$WORK/src.tar.xz" -C "$WORK"
cd "$WORK/ffmpeg-$VERSION"

# LGPL-only, static, and limited to what backend/worker.py uses: decrypting AAX/AAXC (mov demuxer),
# stream-copying AAC into .m4a, and re-encoding the cover to JPEG.
MIN=-mmacosx-version-min=12.0
./configure --prefix="$WORK/install" --disable-everything --disable-autodetect --disable-doc \
  --disable-network --disable-debug --disable-ffplay --enable-static --disable-shared --enable-zlib \
  --extra-cflags="$MIN" --extra-ldflags="$MIN" \
  --enable-demuxer=mov,image2 --enable-muxer=mov,mp4,ipod,image2 \
  --enable-decoder=aac,mjpeg,png --enable-encoder=mjpeg --enable-parser=aac,mjpeg,png \
  --enable-bsf=aac_adtstoasc --enable-protocol=file --enable-filter=scale,format,null \
  >"$WORK/configure.log" || { tail -30 "$WORK/configure.log"; exit 1; }
make -j"$(sysctl -n hw.ncpu)" >"$WORK/make.log" 2>&1 || { tail -30 "$WORK/make.log"; exit 1; }
make install >/dev/null

cd - >/dev/null
rm -rf "$OUT"
mkdir -p "$OUT/bin"
cp "$WORK/install/bin/ffmpeg" "$WORK/install/bin/ffprobe" "$OUT/bin/"
cp "$WORK/ffmpeg-$VERSION/COPYING.LGPLv2.1" "$OUT/LICENSE.txt"
printf 'FFmpeg %s, LGPL v2.1 or later, built by scripts/build-ffmpeg.sh from\nhttps://ffmpeg.org/releases/ffmpeg-%s.tar.xz (sha256 %s).\n' \
  "$VERSION" "$VERSION" "$SHA256" >"$OUT/README.txt"
echo "Built $OUT"
