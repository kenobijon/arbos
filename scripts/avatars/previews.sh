#!/usr/bin/env bash
# Renders MP4 + GIF previews of idle / working / reply for the given slugs.
#   NODE_PATH=<scratch>/node_modules scripts/avatars/previews.sh <outdir> <px> slug[,slug...]
# Frames come from render.js (lottie-web, 60 fps, dark background); ffmpeg
# encodes H.264 MP4 (yuv420p) and a palette GIF at 30 fps.
set -euo pipefail

out="$1"
px="$2"
slugs="$3"
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

node "$here/render.js" frames "$tmp" "$px" "$slugs"
mkdir -p "$out"
for dir in "$tmp"/*/; do
  name="$(basename "$dir")"
  ffmpeg -loglevel error -y -framerate 60 -i "$dir/%04d.png" \
    -c:v libx264 -pix_fmt yuv420p -crf 20 -movflags +faststart "$out/$name.mp4"
  ffmpeg -loglevel error -y -framerate 60 -i "$dir/%04d.png" \
    -vf "fps=30,split[a][b];[a]palettegen=max_colors=64:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=3" \
    -loop 0 "$out/$name.gif"
  printf '%s: %s / %s\n' "$name" "$(du -h "$out/$name.mp4" | cut -f1)" "$(du -h "$out/$name.gif" | cut -f1)"
done
