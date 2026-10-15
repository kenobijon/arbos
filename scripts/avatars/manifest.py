"""Writes assets/avatars/trees/manifest.json.

Run: python scripts/avatars/manifest.py [--ready slug,slug,...|all]
All paths in the manifest are relative to the repository root.
"""

import argparse
import json
from pathlib import Path

from characters import CHARACTERS

REPO = Path(__file__).resolve().parents[2]
ROOT = "assets/avatars/trees"
OUT = REPO / ROOT / "manifest.json"

FPS = 60
STRIP_FPS = 30
# Durations are chosen so the 30 fps frame strips stay within the Mac
# worker's budget: idle/working <= 90 frames, reply <= 30 frames.
STATES = {
    "idle": {"duration_ms": 3000, "loop": True},
    "working": {"duration_ms": 3000, "loop": True},
    "reply": {"duration_ms": 1000, "loop": False},
}
PNG_SIZES = [56, 112, 224, 512]
STRIP_SIZES = {"1x": 28, "2x": 56, "rail": 72, "rail@2x": 144}


def state_frames(state: str, fps: int) -> int:
    return STATES[state]["duration_ms"] * fps // 1000


def strip_path(slug: str, state: str, key: str) -> str:
    suffix = {"1x": "", "2x": "@2x", "rail": "-rail", "rail@2x": "-rail@2x"}[key]
    return f"{ROOT}/frames/{slug}-{state}{suffix}.png"


def character_entry(slug, name, personality, face, ready):
    return {
        "slug": slug,
        "name": name,
        "personality": personality,
        "default_face": face,
        "ready": ready,
        "svg": f"{ROOT}/{slug}.svg",
        "png": {str(s): f"{ROOT}/png/{slug}-{s}.png" for s in PNG_SIZES},
        "lottie": {st: f"{ROOT}/lottie/{slug}/{st}.json" for st in STATES},
        "frames": {
            st: {
                "frames": state_frames(st, STRIP_FPS),
                "fps": STRIP_FPS,
                "loop": STATES[st]["loop"],
                **{key: strip_path(slug, st, key) for key in STRIP_SIZES},
            }
            for st in STATES
        },
    }


def build_manifest(ready_slugs):
    return {
        "schema_version": 1,
        "notes": [
            "Order of `characters` is a contract: the relay picks slugs[fnv1a64(project_id) % n]. Append only.",
            "`ready: false` means the art is a placeholder and the files may not exist yet.",
            "SVG/PNG: 64x64 viewBox, black #000 disc with a 2 px white #fff ring, white tree, black face; transparent outside the disc.",
            "Lottie: 60 fps, shapes only, no expressions, no raster, no masks; `idle` and `working` loop seamlessly, `reply` plays once.",
            "Frame strips: horizontal row of square frames sampled from the Lottie at 30 fps; `1x` = 28 px, `2x` = 56 px, `rail` = 72 px, `rail@2x` = 144 px; transparent outside the disc.",
        ],
        "style": {
            "background": "#000000",
            "foreground": "#ffffff",
            "ring_px": 2,
            "viewbox": [0, 0, 64, 64],
        },
        "default_size": 56,
        "png_sizes": PNG_SIZES,
        "states": {
            st: {
                "fps": FPS,
                "frames": state_frames(st, FPS),
                "duration_ms": cfg["duration_ms"],
                "loop": cfg["loop"],
            }
            for st, cfg in STATES.items()
        },
        "frame_strips": {
            "fps": STRIP_FPS,
            "layout": "horizontal",
            "sizes": STRIP_SIZES,
        },
        "characters": [
            character_entry(slug, name, personality, face, slug in ready_slugs)
            for slug, name, personality, face in CHARACTERS
        ],
    }


def write_manifest(ready_slugs):
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(build_manifest(set(ready_slugs)), indent=2) + "\n")
    return OUT


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ready", default="", help="comma list of slugs, or 'all'")
    args = ap.parse_args()
    ready = (
        [c[0] for c in CHARACTERS]
        if args.ready == "all"
        else [s for s in args.ready.split(",") if s]
    )
    print(write_manifest(ready))


if __name__ == "__main__":
    main()
