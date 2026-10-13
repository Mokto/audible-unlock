#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=9.0.2
SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
OUT="target/ffmpeg-$VERSION-universal"
[[ -x "$OUT/bin/ffmpeg" && -x "$OUT/bin/ffprobe" ]] && exit 0

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
curl -fsSL "https://ffmpeg.org/releases/ffmpeg-$VERSION.tar.xz" -o "$WORK/src.tar.xz"
echo "$SHA256  $WORK/src.tar.xz" | shasum -a 256 -c - >/dev/null
tar -xf "$WORK/src.tar.xz" -C "$WORK"

# LGPL-only, static, and limited to what backend/worker.py uses: decrypting AAX/AAXC (mov demuxer),
# stream-copying AAC into .m4a, and re-encoding the cover to JPEG.
MIN=-mmacosx-version-min=12.0
for ARCH in arm64 x86_64; do
  cp -R "$WORK/ffmpeg-$VERSION" "$WORK/$ARCH"
  CROSS=""
  [[ "$ARCH" == x86_64 ]] && CROSS="--enable-cross-compile --target-os=darwin --disable-x86asm"
  (
    cd "$WORK/$ARCH"
    # shellcheck disable=SC2086
    ./configure --prefix="$WORK/install-$ARCH" --arch="$ARCH" --cc="clang -arch $ARCH" $CROSS \
      --disable-everything --disable-autodetect --disable-doc --disable-network --disable-debug \
      --disable-ffplay --enable-static --disable-shared --enable-zlib \
      --extra-cflags="$MIN" --extra-ldflags="$MIN" \
      --enable-demuxer=mov,image2 --enable-muxer=mov,mp4,ipod,image2 \
      --enable-decoder=aac,mjpeg,png --enable-encoder=mjpeg --enable-parser=aac,mjpeg,png \
      --enable-bsf=aac_adtstoasc --enable-protocol=file --enable-filter=scale,format,null \
      >"$WORK/configure-$ARCH.log" || { tail -30 "$WORK/configure-$ARCH.log"; exit 1; }
    make -j"$(sysctl -n hw.ncpu)" >"$WORK/make-$ARCH.log" 2>&1 || { tail -30 "$WORK/make-$ARCH.log"; exit 1; }
    make install >/dev/null
  )
done

rm -rf "$OUT"
mkdir -p "$OUT/bin"
for TOOL in ffmpeg ffprobe; do
  lipo -create "$WORK/install-arm64/bin/$TOOL" "$WORK/install-x86_64/bin/$TOOL" -output "$OUT/bin/$TOOL"
done
cp "$WORK/ffmpeg-$VERSION/COPYING.LGPLv2.1" "$OUT/LICENSE.txt"
printf 'FFmpeg %s, LGPL v2.1 or later, built by scripts/build-ffmpeg.sh from\nhttps://ffmpeg.org/releases/ffmpeg-%s.tar.xz (sha256 %s).\n' \
  "$VERSION" "$VERSION" "$SHA256" >"$OUT/README.txt"
echo "Built $OUT"
