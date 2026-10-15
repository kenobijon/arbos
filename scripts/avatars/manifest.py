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
# Tree-only mask strips (white tree on transparent, face cut out) for the Mac.
TREE_STRIP_SIZES = {"2x": 56, "rail": 72}

# Per-agent colors. One scheme everywhere: deep tinted disc (dark-mode
# friendly), accent ring, white tree, face in the disc color (cut-out look).
# THE ORDER IS A CONTRACT with the relay (it hashes over this list); the
# slugs are fixed. Append only. slug, name, bg, ring
COLORS = [
    ("mono", "Mono", "#000000", "#ffffff"),
    ("red", "Red", "#2a1210", "#f87171"),
    ("orange", "Orange", "#2b1708", "#fb923c"),
    ("amber", "Amber", "#2a1f08", "#fbbf24"),
    ("green", "Green", "#0f2a1a", "#4ade80"),
    ("teal", "Teal", "#0b2826", "#2dd4bf"),
    ("cyan", "Cyan", "#0b2430", "#22d3ee"),
    ("blue", "Blue", "#0f1f3a", "#60a5fa"),
    ("indigo", "Indigo", "#171a3c", "#818cf8"),
    ("violet", "Violet", "#221540", "#a78bfa"),
    ("pink", "Pink", "#2a1230", "#f472b6"),
    ("rose", "Rose", "#2c1020", "#fb7185"),
    ("slate", "Slate", "#1a1f2a", "#94a3b8"),
]
TREE_INK = "#ffffff"


def color_entry(slug, name, bg, ring):
    return {"slug": slug, "name": name, "bg": bg, "ring": ring, "tree": TREE_INK, "face": bg}


def state_frames(state: str, fps: int) -> int:
    return STATES[state]["duration_ms"] * fps // 1000


def strip_path(slug: str, state: str, key: str) -> str:
    suffix = {"1x": "", "2x": "@2x", "rail": "-rail", "rail@2x": "-rail@2x"}[key]
    return f"{ROOT}/frames/{slug}-{state}{suffix}.png"


def tree_strip_path(slug: str, state: str, key: str) -> str:
    suffix = {"2x": "@2x", "rail": "-rail"}[key]
    return f"{ROOT}/frames/{slug}-{state}-tree{suffix}.png"


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
                "tree": {key: tree_strip_path(slug, st, key) for key in TREE_STRIP_SIZES},
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
            "Tree strips (`frames[state].tree`): same frames, but only the tree as a white mask on transparent with the face cut out; the app draws its own tinted disc + ring behind and may multiply the mask by `tree`.",
            "Colors: one scheme for every entry in `colors` — deep tinted `bg` disc, accent `ring`, white `tree`, `face` = `bg` (cut-out look). `mono` is the default. Order of `colors` is a contract with the relay (hashed like `characters`); append only.",
        ],
        "style": {
            "background": "#000000",
            "foreground": "#ffffff",
            "ring_px": 2,
            "viewbox": [0, 0, 64, 64],
        },
        "colors": [color_entry(*c) for c in COLORS],
        "default_color": "mono",
        "recolor": {
            "roles": ["bg", "ring", "tree", "face"],
            "svg": "Elements carry class=\"bg|ring|tree|face\"; set fill (and stroke for ring and stroke-based face/tree parts) per class.",
            "lottie_layers": {"bg": "disc", "ring": "outline", "tree": "the animated tree"},
            "lottie_keypaths": {
                "bg": "bg.**.bg.Color",
                "ring": "ring.**.ring.Color",
                "tree": "tree.**.tree.Color",
                "face": "tree.**.face.Color",
            },
            "lottie_ios": "animationView.setValueProvider(ColorValueProvider(color.lottieColorValue), keypath: AnimationKeypath(keypath: \"tree.**.face.Color\")) — one call per role; `**` spans the group nesting.",
            "lottie_web": "Before loadAnimation, walk animationData.layers[*].shapes recursively and set `c.k` (RGBA 0-1) on every `fl`/`st` item whose `nm` is the role.",
            "frame_strips": "The white-on-black strips are the `mono` look only. For a color, draw a disc in `bg` with a 2 px `ring` stroke, then the `tree` mask strip on top (multiply by `tree` if it is not white).",
        },
        "default_size": 56,
        "usage": {
            "still": "Use `svg` (or the nearest `png`) wherever the avatar is static: lists, headers, notifications.",
            "idle": "Loop while the agent has nothing running.",
            "working": "Loop while a run is active; switch back to idle when it ends (both loops share frame 0, so cut on a loop boundary).",
            "reply": "Play once when a reply lands, then return to idle. It starts and ends on the idle pose.",
            "frame_strips": "Frame k of a strip is the square at x = k * size. Advance at `fps`; loop idle/working, play reply once. The widest strip (rail@2x, 90 frames) is 12960 px wide.",
            "tree_strips": "`frames[state].tree` = white mask strips at 56 (`2x`) and 72 (`rail`) px with the face cut out. Draw the tinted disc + ring, then the mask. See `recolor`.",
            "color": "Per-agent color = one entry of `colors`; default `mono`. Apply the same entry to the still, the Lotties and the strips.",
            "pick": "Default avatar for a project = characters[fnv1a64(project_id) % characters.length].",
        },
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
