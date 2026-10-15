"""Geometry of the 20 tree characters and their faces (64x64 space).

Chunky mascot style: thick stubby trunks, plump puffy canopies, soft rounded
silhouettes. Ink keys: w = tree base (canopy), d = tree dark (trunk, shade),
l = tree light (highlight), k = face. In mono every tree ink is white.

A Tree is: canopy shapes (drawn bottom to top, highlights last), trunk shapes
(drawn under the canopy), a face anchor, and a pivot for the canopy
"breathing" scale.
"""

import math
from dataclasses import dataclass, field

from shapes import (
    Circle,
    Rect,
    blob,
    curve,
    ellipse,
    half_disc,
    line,
    round_poly,
)

SWAY_PIVOT = (32.0, 54.0)

# "Thinking" marks for the working state: three dots growing outward in the
# black space to the upper right, clear of every canopy and inside the ring.
SPARKLES = [((40.5, 7.0), 1.4), ((49.5, 11.5), 1.8), ((56.0, 19.5), 2.2)]


@dataclass
class Face:
    cx: float
    cy: float
    scale: float = 1.0
    gap: float = 1.0  # multiplies the eye spacing (narrow trees use < 1)


@dataclass
class Tree:
    canopy: list
    trunk: list
    face: Face
    canopy_pivot: tuple
    canopy_black: list = field(default_factory=list)  # face-ink detail drawn over the canopy


# ---------- faces ----------
# Faces are 2-3 strokes/dots in face ink on the canopy. Eyes are always a
# separate group (blink / look-around); mouths come as a default and a
# "reply" variant (big open smile) that the animation swaps in.


def face_parts(kind: str, f: Face):
    s, cx, cy = f.scale, f.cx, f.cy
    dx = 4.3 * s * f.gap
    ey = cy - 1.6 * s
    r = 2.1 * s
    w = max(1.6, 2.0 * s)
    lx, rx = cx - dx, cx + dx

    def dot(x, big=1.0):
        return Circle(x, ey, r * big, fill="k")

    def closed_eye(x):
        return curve((x - 2.1 * s, ey + 0.7 * s), (x + 2.1 * s, ey + 0.7 * s), -1.9 * s, "k", w)

    def narrow_eye(x):
        return ellipse(x, ey + 0.4 * s, 2.3 * s, 1.4 * s, fill="k")

    def lid(x):
        return line([(x - 2.4 * s, ey - 2.2 * s), (x + 2.4 * s, ey - 2.2 * s)], "k", w * 0.9)

    def brow(x, tilt):
        # tilt > 0 lowers the inner end (toward cx): determined look
        inner = 1 if x < cx else -1
        y0 = ey - 3.9 * s
        return line(
            [(x - 2.2 * s, y0 + (tilt if inner < 0 else 0)), (x + 2.2 * s, y0 + (tilt if inner > 0 else 0))],
            "k",
            w * 0.9,
        )

    smile = curve((cx - 3.6 * s, cy + 2.6 * s), (cx + 3.6 * s, cy + 2.6 * s), 2.4 * s, "k", w)
    small_smile = curve((cx - 2.6 * s, cy + 2.8 * s), (cx + 2.6 * s, cy + 2.8 * s), 1.5 * s, "k", w)
    flat = line([(cx - 2.6 * s, cy + 3.1 * s), (cx + 2.6 * s, cy + 3.1 * s)], "k", w)
    short = line([(cx - 1.6 * s, cy + 3.2 * s), (cx + 1.6 * s, cy + 3.2 * s)], "k", w)
    half_smile = curve((cx - 3.0 * s, cy + 3.3 * s), (cx + 3.6 * s, cy + 2.2 * s), 1.6 * s, "k", w)

    eyes, brows, mouth = [], [], []
    if kind == "happy":
        eyes, mouth = [dot(lx), dot(rx)], [smile]
    elif kind == "calm":
        eyes, mouth = [closed_eye(lx), closed_eye(rx)], [small_smile]
    elif kind == "focused":
        eyes, brows, mouth = [narrow_eye(lx), narrow_eye(rx)], [lid(lx), lid(rx)], [short]
    elif kind == "determined":
        eyes, brows, mouth = [dot(lx), dot(rx)], [brow(lx, 1.1 * s), brow(rx, 1.1 * s)], [flat]
    elif kind == "winking":
        eyes, mouth = [dot(lx), closed_eye(rx)], [half_smile]
    elif kind == "surprised":
        eyes, mouth = [dot(lx, 1.2), dot(rx, 1.2)], [Circle(cx, cy + 3.4 * s, 1.5 * s, fill="k")]
    else:
        raise ValueError(kind)

    reply_mouth = [half_disc(cx, cy + 2.2 * s, 4.2 * s, 3.4 * s, lower=True, fill="k")]
    return {
        "eyes": eyes,
        "eye_center": (cx, ey),
        "brows": brows,
        "mouth": mouth,
        "reply_mouth": reply_mouth,
    }


# ---------- building blocks ----------


def stub(cx, top, bottom, w, flare=1.5, r=3.0, ink="d"):
    """Thick stubby trunk: a rounded trapezoid that flares a little at the base."""
    return round_poly(
        [(cx - w / 2, top), (cx + w / 2, top), (cx + w / 2 + flare, bottom), (cx - w / 2 - flare, bottom)],
        r,
        fill=ink,
    )


def shine(cx, cy, rx=4.5, ry=3.0, rot=-30):
    """Highlight blob (tree_light), usually upper-left on the canopy."""
    return ellipse(cx, cy, rx, ry, rot, fill="l")


def tier(tip, half_width, base_y, r=3.5):
    """Plump rounded triangle for conifers."""
    tx, ty = tip
    return round_poly([(tx, ty), (tx + half_width, base_y), (tx - half_width, base_y)], r)


# ---------- trees ----------


def oak():
    canopy = [
        Circle(32, 18, 12.5),
        Circle(18.5, 27, 10),
        Circle(45.5, 27, 10),
        Circle(25, 34, 9.5),
        Circle(39, 34, 9.5),
        shine(24, 13),
    ]
    trunk = [stub(32, 40, 54, 11)]
    return Tree(canopy, trunk, Face(32, 27, 1.05), (32, 43))


def pine():
    canopy = [
        tier((32, 6), 11, 22),
        tier((32, 13), 16, 33),
        tier((32, 22), 20, 43),
        shine(28, 16, 2.4, 4, 25),
    ]
    trunk = [stub(32, 42, 54, 10)]
    return Tree(canopy, trunk, Face(32, 34, 1.0, 0.9), (32, 43))


def lollipop():
    canopy = [Circle(32, 25, 17), shine(24, 15, 5, 3.5, -35)]
    trunk = [stub(32, 40, 54, 10)]
    return Tree(canopy, trunk, Face(32, 26, 1.1), (32, 42))


def fractal():
    canopy = [
        Circle(32, 20, 11),
        Circle(18, 26, 8),
        Circle(46, 26, 8),
        Circle(11, 19, 4.5),
        Circle(19, 13, 4.5),
        Circle(53, 19, 4.5),
        Circle(45, 13, 4.5),
        Circle(27, 7, 4),
        Circle(37, 7, 4),
        shine(27, 13, 3, 2, -30),
    ]
    trunk = [
        stub(32, 36, 54, 10),
        line([(32, 38), (19, 28)], "d", 6),
        line([(32, 38), (45, 28)], "d", 6),
        line([(32, 38), (32, 24)], "d", 7),
    ]
    return Tree(canopy, trunk, Face(32, 21, 1.0), (32, 40))


def mushroom():
    canopy = [
        blob([(32, 8), (47, 13), (53, 27), (44, 32), (20, 32), (11, 27), (17, 13)], 0.9),
        Circle(20, 21, 3, fill="l"),
        Circle(41, 13, 2.6, fill="l"),
        Circle(46, 24, 2.2, fill="l"),
    ]
    trunk = [round_poly([(25.5, 30), (38.5, 30), (39.5, 54), (24.5, 54)], 5, fill="d")]
    return Tree(canopy, trunk, Face(32, 22, 1.0), (32, 32))


def cypress():
    canopy = [
        blob([(32, 5), (43, 16), (45.5, 30), (41, 43), (32, 46), (23, 43), (18.5, 30), (21, 16)], 0.95),
        shine(27, 17, 2.4, 5, 15),
    ]
    trunk = [stub(32, 44, 54, 9)]
    return Tree(canopy, trunk, Face(32, 28, 1.0, 0.8), (32, 46))


def willow():
    canopy = [
        ellipse(32, 20, 18, 12.5),
        ellipse(15.5, 29, 4, 8.5),
        ellipse(23, 32, 4, 9.5),
        ellipse(41, 32, 4, 9.5),
        ellipse(48.5, 29, 4, 8.5),
        shine(23, 12, 5, 3, -30),
    ]
    trunk = [stub(32, 30, 54, 10)]
    return Tree(canopy, trunk, Face(32, 20, 1.05), (32, 32))


def palm():
    canopy = [
        ellipse(22, 16, 11.5, 5.2, -40),
        ellipse(44, 16, 11.5, 5.2, 40),
        ellipse(18, 26, 11.5, 5.2, -8),
        ellipse(46, 26, 11.5, 5.2, 8),
        ellipse(32, 12, 5.2, 10.5, 0),
        Circle(32, 25, 9.5),
        shine(24, 15, 3.2, 1.8, -40),
    ]
    trunk = [round_poly([(28, 54), (40, 54), (36, 28), (30, 28)], 3, fill="d")]
    return Tree(canopy, trunk, Face(32, 24, 0.9, 0.9), (32, 34))


def bonsai():
    canopy = [
        ellipse(37, 27, 13, 8),
        ellipse(20, 33, 9, 5.5),
        ellipse(28, 16, 8, 5),
        shine(31, 22, 3.5, 2, -25),
    ]
    trunk = [
        round_poly([(19, 45), (45, 45), (42, 54), (22, 54)], 2.5, fill="d"),
        curve((31, 46), (36, 32), 3, "d", 7),
        line([(33, 39), (23, 34)], "d", 4),
        line([(35, 25), (29, 18)], "d", 3.5),
    ]
    return Tree(canopy, trunk, Face(38, 27, 0.95, 0.9), (32, 46))


def birch():
    canopy = [ellipse(32, 22, 13, 17), shine(26, 11, 4, 3, -30)]
    trunk = [
        stub(32, 36, 54, 9, ink="l"),
        Rect(28, 41, 4, 2, 1, fill="d"),
        Rect(32, 46, 4, 2, 1, fill="d"),
        Rect(28, 51, 4, 2, 1, fill="d"),
    ]
    return Tree(canopy, trunk, Face(32, 24, 1.05, 0.85), (32, 39))


def baobab():
    canopy = [
        ellipse(32, 19, 18, 6),
        Circle(20, 18, 5.5),
        Circle(44, 18, 5.5),
        Circle(32, 14, 6.5),
        shine(24, 13, 3.5, 2, -20),
    ]
    trunk = [round_poly([(19, 54), (45, 54), (40, 24), (24, 24)], 6, fill="w")]
    return Tree(canopy, trunk, Face(32, 40, 1.0, 0.9), (32, 24))


def sapling():
    canopy = [
        Circle(32, 28, 10),
        Circle(24, 33, 7),
        Circle(40, 33, 7),
        shine(27, 22, 3, 2, -30),
    ]
    trunk = [
        half_disc(32, 54, 10, 3.5, lower=False, fill="d"),
        stub(32, 38, 54, 7, 1),
    ]
    return Tree(canopy, trunk, Face(32, 29, 0.9), (32, 40))


def maple():
    pts = []
    cx, cy = 32, 25
    for k in range(10):
        a = math.radians(-90 + k * 36)
        rad = 20 if k % 2 == 0 else 13
        pts.append((cx + rad * math.cos(a), cy + rad * math.sin(a)))
    canopy = [round_poly(pts, 4), shine(26, 15, 3.5, 2.5, -35)]
    trunk = [stub(32, 40, 54, 10)]
    return Tree(canopy, trunk, Face(32, 27, 1.05), (32, 43))


def acacia():
    canopy = [
        round_poly([(10, 20), (54, 20), (48, 32), (16, 32)], 6),
        ellipse(32, 20, 22, 5),
        shine(20, 21, 4, 2, -10),
    ]
    trunk = [
        stub(32, 30, 54, 9),
        line([(32, 40), (22, 30)], "d", 6),
        line([(32, 40), (42, 30)], "d", 6),
    ]
    return Tree(canopy, trunk, Face(32, 25, 0.85, 1.1), (32, 32))


def spruce():
    canopy = [
        tier((32, 5), 8, 17, 3),
        tier((32, 10), 12, 25, 3),
        tier((32, 17), 16, 34, 3),
        tier((32, 25), 19, 44, 3),
        shine(29, 12, 2, 3.5, 20),
    ]
    trunk = [stub(32, 43, 54, 9)]
    return Tree(canopy, trunk, Face(32, 35, 0.9, 0.85), (32, 44))


def cherry():
    canopy = [
        Circle(32, 17, 11),
        Circle(19, 25, 9.5),
        Circle(45, 25, 9.5),
        Circle(25, 32, 8),
        Circle(39, 32, 8),
        ellipse(20, 36, 3.5, 6.5),
        ellipse(44, 36, 3.5, 6.5),
        Circle(21, 19, 2, fill="l"),
        Circle(44, 17, 2, fill="l"),
        Circle(30, 10, 2, fill="l"),
        Circle(48, 30, 1.8, fill="l"),
        Circle(16, 32, 1.8, fill="l"),
    ]
    trunk = [stub(32, 38, 54, 10)]
    return Tree(canopy, trunk, Face(32, 26, 1.05), (32, 40))


def cactus():
    canopy = [
        Rect(25.5, 12, 13, 42, 6.5),
        Rect(13, 22, 7, 16, 3.5),
        Rect(13, 31, 15, 7, 3.5),
        Rect(44, 17, 7, 18, 3.5),
        Rect(36, 28, 15, 7, 3.5),
        Circle(32, 12.5, 3, fill="l"),
    ]
    trunk = [half_disc(32, 54, 13, 3.5, lower=False, fill="d")]
    return Tree(canopy, trunk, Face(32, 27, 1.0, 0.8), (32, 54))


def sequoia():
    canopy = [
        round_poly([(32, 4), (38, 10), (42, 22), (45, 34), (47, 44), (17, 44), (19, 34), (22, 22), (26, 10)], 5),
        shine(28, 15, 2.2, 4.5, 15),
    ]
    trunk = [stub(32, 43, 54, 13, 2)]
    return Tree(canopy, trunk, Face(32, 32, 1.0, 0.85), (32, 45))


def olive():
    canopy = [
        Circle(21, 26, 8.5),
        Circle(43, 22, 9),
        Circle(33, 30, 9.5),
        Circle(29, 16, 7.5),
        Circle(47, 32, 6),
        Circle(15, 18, 4.5),
        shine(26, 11, 3.5, 2, -30),
    ]
    trunk = [
        curve((31, 54), (30, 34), -2.5, "d", 8),
        line([(31, 46), (40, 36)], "d", 5),
    ]
    return Tree(canopy, trunk, Face(34, 27, 1.0, 0.9), (32, 38))


def ginkgo():
    canopy = [
        blob(
            [(32, 38), (20, 31), (9, 21), (12, 12), (22, 8.5), (32, 10), (42, 8.5), (52, 12), (55, 21), (44, 31)],
            0.6,
        ),
        shine(22, 14, 3.5, 2.2, -30),
    ]
    trunk = [stub(32, 37, 54, 8)]
    return Tree(
        canopy,
        trunk,
        Face(32, 25, 1.0),
        (32, 38),
        canopy_black=[
            line([(32, 38), (32, 31.5)], "k", 1.4),
            line([(32, 38), (17.5, 15)], "k", 1.2),
            line([(32, 38), (46.5, 15)], "k", 1.2),
        ],
    )


TREES = {
    "oak": oak,
    "pine": pine,
    "lollipop": lollipop,
    "fractal": fractal,
    "mushroom": mushroom,
    "cypress": cypress,
    "willow": willow,
    "palm": palm,
    "bonsai": bonsai,
    "birch": birch,
    "baobab": baobab,
    "sapling": sapling,
    "maple": maple,
    "acacia": acacia,
    "spruce": spruce,
    "cherry": cherry,
    "cactus": cactus,
    "sequoia": sequoia,
    "olive": olive,
    "ginkgo": ginkgo,
}
