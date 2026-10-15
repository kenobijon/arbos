#!/usr/bin/env python3
"""Compose the README hero: a Mac window with the iPhone overlapping in front, dark backdrop.

Inputs (both real captures, 2x):
  --mac     screenshot of the Arbos Mac window taken with `screencapture -l <id> -o -x` on a
            Retina (2x) display: rounded corners are transparent, no shadow.
  --iphone  `xcrun simctl io <udid> screenshot` of the iPhone app (3x, no device frame).

Output: a 2x PNG (default 2400x1500, shown at half size by GitHub), quantized with pngquant
when it is installed so the file stays small (target < 1 MB).

    python3 scripts/readme-hero/compose.py --mac mac.png --iphone iphone.png \
        --out docs/assets/hero.png

Needs Pillow (`pip install pillow`). See capture-mac.sh / capture-iphone.sh for the inputs.
"""

from __future__ import annotations

import argparse
import math
import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

BACKGROUND = (10, 10, 10)
BACKGROUND_GLOW = (28, 28, 30)


def backdrop(width: int, height: int) -> Image.Image:
    """Near-black canvas with a soft glow behind the window so the composite has depth."""
    base = Image.new("RGB", (width, height), BACKGROUND)
    glow = Image.new("L", (width, height), 0)
    draw = ImageDraw.Draw(glow)
    cx, cy = width * 0.5, height * 0.42
    rx, ry = width * 0.62, height * 0.6
    draw.ellipse((cx - rx, cy - ry, cx + rx, cy + ry), fill=255)
    glow = glow.filter(ImageFilter.GaussianBlur(width * 0.12))
    tint = Image.new("RGB", (width, height), BACKGROUND_GLOW)
    return Image.composite(tint, base, glow)


def shadow(size: tuple[int, int], radius: int, blur: int, alpha: int) -> Image.Image:
    """A blurred rounded rectangle used as a drop shadow; `blur` px of padding on every side."""
    pad = blur * 3
    layer = Image.new("RGBA", (size[0] + 2 * pad, size[1] + 2 * pad), (0, 0, 0, 0))
    ImageDraw.Draw(layer).rounded_rectangle(
        (pad, pad, pad + size[0], pad + size[1]), radius=radius, fill=(0, 0, 0, alpha)
    )
    return layer.filter(ImageFilter.GaussianBlur(blur))


def paste_with_shadow(
    canvas: Image.Image, layer: Image.Image, xy: tuple[int, int], *, radius: int, blur: int, alpha: int, offset_y: int
) -> None:
    sh = shadow(layer.size, radius, blur, alpha)
    pad = blur * 3
    canvas.alpha_composite(sh, (xy[0] - pad, xy[1] - pad + offset_y))
    canvas.alpha_composite(layer, xy)


def rounded_mask(size: tuple[int, int], radius: int, supersample: int = 4) -> Image.Image:
    big = Image.new("L", (size[0] * supersample, size[1] * supersample), 0)
    ImageDraw.Draw(big).rounded_rectangle(
        (0, 0, big.width - 1, big.height - 1), radius=radius * supersample, fill=255
    )
    return big.resize(size, Image.Resampling.LANCZOS)


def mac_window(path: Path, target_width: int) -> tuple[Image.Image, int]:
    """The window capture scaled to `target_width`; corners are re-masked so they stay crisp."""
    src = Image.open(path).convert("RGBA")
    scale = target_width / src.width
    size = (target_width, round(src.height * scale))
    win = src.resize(size, Image.Resampling.LANCZOS)
    radius = round(26 * scale)  # macOS window corner at 2x is ~26 px
    mask = rounded_mask(size, radius)
    # Keep the capture's own transparency (its rounded corners) AND the clean mask.
    win.putalpha(ImageChops.multiply(win.getchannel("A"), mask))
    return win, radius


def iphone(path: Path, target_height: int) -> tuple[Image.Image, int]:
    """The simulator screenshot inside a simple device frame (bezel + rounded corners)."""
    screen = Image.open(path).convert("RGBA")
    scale = target_height / screen.height
    screen_size = (round(screen.width * scale), target_height)
    screen = screen.resize(screen_size, Image.Resampling.LANCZOS)

    bezel = round(screen_size[0] * 0.028)
    screen_radius = round(screen_size[0] * 0.145)
    frame_radius = screen_radius + bezel
    frame_size = (screen_size[0] + 2 * bezel, screen_size[1] + 2 * bezel)

    frame = Image.new("RGBA", frame_size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(frame)
    draw.rounded_rectangle((0, 0, frame_size[0] - 1, frame_size[1] - 1), radius=frame_radius, fill=(22, 22, 24, 255))
    edge = max(2, bezel // 6)
    draw.rounded_rectangle(
        (0, 0, frame_size[0] - 1, frame_size[1] - 1), radius=frame_radius, outline=(70, 70, 74, 255), width=edge
    )
    frame.putalpha(ImageChops.multiply(frame.getchannel("A"), rounded_mask(frame_size, frame_radius)))

    screen.putalpha(rounded_mask(screen_size, screen_radius))
    frame.alpha_composite(screen, (bezel, bezel))
    return frame, frame_radius


def compose(mac_path: Path, iphone_path: Path, width: int, height: int) -> Image.Image:
    canvas = backdrop(width, height).convert("RGBA")

    win, win_radius = mac_window(mac_path, target_width=round(width * 0.80))
    win_x = round(width * 0.05)
    win_y = round(height * 0.075)
    paste_with_shadow(canvas, win, (win_x, win_y), radius=win_radius, blur=round(width * 0.02), alpha=170, offset_y=round(height * 0.02))

    phone, phone_radius = iphone(iphone_path, target_height=round(height * 0.72))
    phone_x = width - phone.width - round(width * 0.045)
    phone_y = height - phone.height - round(height * 0.045)
    paste_with_shadow(canvas, phone, (phone_x, phone_y), radius=phone_radius, blur=round(width * 0.018), alpha=200, offset_y=round(height * 0.015))

    return canvas.convert("RGB")


def write_optimized(image: Image.Image, out: Path, quantize: bool) -> None:
    out.parent.mkdir(parents=True, exist_ok=True)
    image.save(out, "PNG", optimize=True)
    if quantize and shutil.which("pngquant"):
        subprocess.run(
            ["pngquant", "--force", "--skip-if-larger", "--quality", "70-95", "--speed", "1", "--output", str(out), str(out)],
            check=False,
        )
    elif quantize:
        print("pngquant not found; writing a 256-colour PNG with Pillow instead", file=sys.stderr)
        image.quantize(colors=256, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.FLOYDSTEINBERG).save(out, "PNG", optimize=True)
    size_kb = out.stat().st_size / 1024
    print(f"{out} {image.width}x{image.height} {size_kb:.0f} KB")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--mac", required=True, type=Path, help="Mac window capture (2x, transparent corners)")
    ap.add_argument("--iphone", required=True, type=Path, help="iPhone simulator screenshot")
    ap.add_argument("--out", default=Path("docs/assets/hero.png"), type=Path)
    ap.add_argument("--width", type=int, default=2400, help="canvas width in px (2x)")
    ap.add_argument("--height", type=int, default=0, help="canvas height in px; default keeps 16:10")
    ap.add_argument("--no-quantize", action="store_true", help="keep the full-colour PNG")
    args = ap.parse_args()

    height = args.height or math.floor(args.width * 10 / 16)
    image = compose(args.mac, args.iphone, args.width, height)
    write_optimized(image, args.out, quantize=not args.no_quantize)


if __name__ == "__main__":
    main()
