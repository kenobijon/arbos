"""Builds the tree avatar assets from the geometry in trees.py.

    source .venv/bin/activate   # needs: cairosvg pillow
    python scripts/avatars/build.py [--only oak,pine] [--contact-sheet PATH]

Writes, under assets/avatars/trees/:
    <slug>.svg                      still, 64x64 viewBox
    png/<slug>-{56,112,224,512}.png still exports (transparent outside the disc)
    lottie/<slug>/{idle,working,reply}.json
    manifest.json                   (characters marked ready when their files exist)

Frame strips and video previews come from render.js (lottie-web in headless
Chromium), which reads the Lottie files this script writes.
"""

import argparse
import io
import json
import re
from pathlib import Path

import cairosvg
from PIL import Image, ImageDraw, ImageFont

from characters import CHARACTERS
from lottie import DURATION, tree_lottie
from manifest import OUT as MANIFEST_PATH
from manifest import COLORS, PNG_SIZES, color_entry, write_manifest
from svg import tree_svg
from trees import TREES

REPO = Path(__file__).resolve().parents[2]
ROOT = REPO / "assets/avatars/trees"
LOTTIE_BUDGET = 25 * 1024


def minified(obj):
    return json.dumps(obj, separators=(",", ":"))


def build_character(slug, name, personality, face):
    svg = tree_svg(slug, name)
    (ROOT / f"{slug}.svg").write_text(svg)
    (ROOT / "png").mkdir(exist_ok=True)
    for size in PNG_SIZES:
        cairosvg.svg2png(
            bytestring=svg.encode(),
            output_width=size,
            output_height=size,
            write_to=str(ROOT / "png" / f"{slug}-{size}.png"),
        )
    tree = TREES[slug]()
    ldir = ROOT / "lottie" / slug
    ldir.mkdir(parents=True, exist_ok=True)
    sizes = {}
    for state in DURATION:
        data = minified(tree_lottie(slug, name, face, tree, state))
        if len(data) > LOTTIE_BUDGET:
            raise SystemExit(f"{slug}/{state}.json is {len(data)} bytes, over the 25 KB budget")
        (ldir / f"{state}.json").write_text(data)
        sizes[state] = len(data)
    return sizes


def recolor_svg(svg: str, color: dict) -> str:
    """Retints a still by role class, the same way an app would."""

    def retint(match):
        element = match.group(0)
        role = re.search(r'class="(\w+)"', element).group(1)
        hexv = color[role]
        element = re.sub(r'fill="#[0-9a-fA-F]{6}"', f'fill="{hexv}"', element)
        return re.sub(r'stroke="#[0-9a-fA-F]{6}"', f'stroke="{hexv}"', element)

    return re.sub(r"<(?:circle|rect|path)[^>]*class=\"\w+\"[^>]*/>", retint, svg)


def color_sheet(path, slugs, color_slugs, cell=112):
    """Grid: one row per tree, one column per color, each tile retinted by role."""
    colors = {c[0]: color_entry(*c) for c in COLORS}
    pad, label_w, header_h = 18, 110, 34
    w = label_w + len(color_slugs) * (cell + pad) + pad
    h = header_h + len(slugs) * (cell + pad) + pad
    sheet = Image.new("RGBA", (w, h), (18, 18, 18, 255))
    draw = ImageDraw.Draw(sheet)
    font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 14)
    names = {c[0]: c[1] for c in CHARACTERS}
    for j, cslug in enumerate(color_slugs):
        x = label_w + j * (cell + pad)
        draw.text((x + cell / 2, 10), colors[cslug]["name"], fill=(200, 200, 200), font=font, anchor="ma")
    for i, slug in enumerate(slugs):
        y = header_h + i * (cell + pad)
        draw.text((12, y + cell / 2), names[slug], fill=(200, 200, 200), font=font, anchor="lm")
        svg = (ROOT / f"{slug}.svg").read_text()
        for j, cslug in enumerate(color_slugs):
            x = label_w + j * (cell + pad)
            png = cairosvg.svg2png(bytestring=recolor_svg(svg, colors[cslug]).encode(), output_width=cell, output_height=cell)
            sheet.alpha_composite(Image.open(io.BytesIO(png)), (x, y))
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    sheet.save(path)
    return path


def ready_slugs():
    out = []
    for slug, *_ in CHARACTERS:
        files = [ROOT / f"{slug}.svg"] + [ROOT / "lottie" / slug / f"{s}.json" for s in DURATION]
        if all(f.exists() for f in files):
            out.append(slug)
    return out


def contact_sheet(path, cell=112, cols=5):
    """Dark sheet of every ready character at `cell` px with its name below."""
    chars = [c for c in CHARACTERS if (ROOT / f"{c[0]}.svg").exists()]
    rows = (len(chars) + cols - 1) // cols
    pad, label_h = 28, 44
    w = pad + cols * (cell + pad)
    h = pad + rows * (cell + label_h + pad)
    sheet = Image.new("RGBA", (w, h), (18, 18, 18, 255))
    draw = ImageDraw.Draw(sheet)
    font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 15)
    small = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 11)
    for k, (slug, name, personality, _face) in enumerate(chars):
        svg = (ROOT / f"{slug}.svg").read_bytes()
        png = cairosvg.svg2png(bytestring=svg, output_width=cell, output_height=cell)
        x = pad + (k % cols) * (cell + pad)
        y = pad + (k // cols) * (cell + label_h + pad)
        sheet.alpha_composite(Image.open(io.BytesIO(png)), (x, y))
        draw.text((x + cell / 2, y + cell + 8), name, fill=(240, 240, 240), font=font, anchor="ma")
        draw.text((x + cell / 2, y + cell + 27), f"{slug} · {personality}", fill=(150, 150, 150), font=small, anchor="ma")
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    sheet.save(path)
    return path


def optimize_frames():
    """Re-encodes the frame strips render.js wrote with zlib level 9 (~30% smaller)."""
    total = 0
    for f in sorted((ROOT / "frames").glob("*.png")):
        Image.open(f).save(f, optimize=True)
        total += f.stat().st_size
    print(f"frames: {total / 1e6:.1f} MB after optimize")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", default="", help="comma list of slugs (default: all)")
    ap.add_argument("--contact-sheet", default="", help="also write a contact sheet PNG here")
    ap.add_argument("--optimize-frames", action="store_true", help="only re-compress frames/*.png")
    ap.add_argument("--color-sheet", default="", help="write a trees x colors sheet here (with --only and --colors)")
    ap.add_argument("--colors", default="mono,red,amber,green,blue,violet", help="color slugs for --color-sheet")
    args = ap.parse_args()
    if args.optimize_frames:
        optimize_frames()
        return
    if args.color_sheet:
        print("color sheet:", color_sheet(args.color_sheet, args.only.split(","), args.colors.split(",")))
        return
    wanted = set(args.only.split(",")) if args.only else {c[0] for c in CHARACTERS}
    ROOT.mkdir(parents=True, exist_ok=True)
    for slug, name, personality, face in CHARACTERS:
        if slug in wanted:
            sizes = build_character(slug, name, personality, face)
            print(f"{slug:9s} lottie bytes: " + ", ".join(f"{k}={v}" for k, v in sizes.items()))
    write_manifest(ready_slugs())
    print(f"manifest: {MANIFEST_PATH} ready={len(ready_slugs())}/{len(CHARACTERS)}")
    if args.contact_sheet:
        print("contact sheet:", contact_sheet(args.contact_sheet))


if __name__ == "__main__":
    main()
