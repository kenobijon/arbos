"""Geometry of the 20 tree characters and their faces (64x64 space).

A Tree is: white canopy shapes, white trunk shapes (plus any black detail),
a face anchor, a pivot for the canopy "breathing" scale, and three points
near the canopy where the `working` sparkles appear.
"""

import math
from dataclasses import dataclass, field

from shapes import (
    Circle,
    Rect,
    blob,
    cone,
    curve,
    ellipse,
    half_disc,
    line,
    polygon,
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
    canopy_black: list = field(default_factory=list)  # black detail drawn over the canopy


# ---------- faces ----------
# Faces are 2-3 strokes/dots, black on the white canopy. Eyes are always a
# separate group (blink / look-around); mouths come as a default and a
# "reply" variant (big open smile) that the animation swaps in.


def face_parts(kind: str, f: Face):
    s, cx, cy = f.scale, f.cx, f.cy
    dx = 4.3 * s * f.gap
    ey = cy - 1.6 * s
    r = 2.0 * s
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


# ---------- trees ----------


def oak():
    canopy = [
        Circle(32, 19, 12),
        Circle(21, 27, 10.5),
        Circle(43, 27, 10.5),
        Circle(32, 29, 13),
    ]
    trunk = [round_poly([(28.5, 36), (35.5, 36), (37.5, 54), (26.5, 54)], 1.5)]
    return Tree(canopy, trunk, Face(32, 28), (32, 40))


def pine():
    canopy = [
        cone((32, 7), 9, 20),
        cone((32, 13), 13, 30),
        cone((32, 21), 17, 41),
    ]
    trunk = [Rect(29.5, 40, 5, 14, 1.5)]
    return Tree(canopy, trunk, Face(32, 34, 0.9, 0.85), (32, 41))


def lollipop():
    canopy = [Circle(32, 24, 16)]
    trunk = [Rect(30.5, 38, 3, 16, 1.5)]
    return Tree(canopy, trunk, Face(32, 25), (32, 40))


def fractal():
    canopy = [
        # branch strokes first; the circles sit on top
        line([(32, 40), (32, 30)], "w", 3),
        line([(32, 30), (20, 22)], "w", 2.4),
        line([(32, 30), (44, 22)], "w", 2.4),
        line([(20, 22), (13, 15)], "w", 1.8),
        line([(20, 22), (24, 11)], "w", 1.8),
        line([(44, 22), (51, 15)], "w", 1.8),
        line([(44, 22), (40, 11)], "w", 1.8),
        Circle(32, 29, 9.5),
        Circle(20, 22, 5),
        Circle(44, 22, 5),
        Circle(13, 15, 3),
        Circle(24, 11, 3),
        Circle(51, 15, 3),
        Circle(40, 11, 3),
    ]
    trunk = [Rect(30, 38, 4, 16, 1.5)]
    return Tree(canopy, trunk, Face(32, 29, 0.85), (32, 40))


def mushroom():
    canopy = [
        blob([(32, 10), (47, 17), (51, 29), (32, 31), (13, 29), (17, 17)], 0.9),
    ]
    trunk = [round_poly([(26, 31), (38, 31), (37, 54), (27, 54)], 3)]
    return Tree(canopy, trunk, Face(32, 21, 0.95), (32, 31))


def cypress():
    canopy = [
        blob([(32, 5), (41, 24), (39, 40), (32, 45), (25, 40), (23, 24)], 1.1),
    ]
    trunk = [Rect(30, 44, 4, 10, 1.5)]
    return Tree(canopy, trunk, Face(32, 28, 0.9, 0.75), (32, 45))


def willow():
    canopy = [
        ellipse(32, 19, 18, 11.5),
        curve((15.5, 22), (12.5, 36), -1.2, "w", 1.9),
        curve((19.5, 27), (18, 42), -1.0, "w", 1.9),
        curve((24, 29.5), (23, 46), -0.6, "w", 1.9),
        curve((40, 29.5), (41, 46), 0.6, "w", 1.9),
        curve((44.5, 27), (46, 42), 1.0, "w", 1.9),
        curve((48.5, 22), (51.5, 36), 1.2, "w", 1.9),
    ]
    trunk = [Rect(29.5, 29, 5, 25, 1.5)]
    return Tree(canopy, trunk, Face(32, 19, 0.95), (32, 31))


def palm():
    fronds = [
        ellipse(21, 17, 12, 4, -42),
        ellipse(43, 17, 12, 4, 42),
        ellipse(17.5, 27, 11, 3.8, -8),
        ellipse(46.5, 27, 11, 3.8, 8),
        ellipse(32, 12, 3.8, 10, 0),
    ]
    canopy = fronds + [Circle(32, 25, 8.5)]
    trunk = [curve((33, 54), (32, 30), 2.5, "w", 4.5)]
    return Tree(canopy, trunk, Face(32, 25, 0.85, 0.9), (32, 32))


def bonsai():
    canopy = [
        curve((31, 46), (36, 30), 3, "w", 3.6),
        line([(34, 36), (24, 33)], "w", 2.4),
        ellipse(21, 32, 8.5, 4.6),
        ellipse(37, 26, 13, 7.2),
        ellipse(29, 15, 7, 4.2),
        line([(36, 22), (30, 17)], "w", 2.2),
    ]
    trunk = [round_poly([(21, 45), (43, 45), (40, 54), (24, 54)], 1.5)]
    return Tree(canopy, trunk, Face(38, 26, 0.85, 0.9), (32, 46))


def birch():
    canopy = [ellipse(32, 23, 11, 16)]
    trunk = [
        Rect(30, 38, 4, 16, 1.5),
        Rect(30, 42, 2, 1.6, 0, fill="k"),
        Rect(32, 46.5, 2, 1.6, 0, fill="k"),
        Rect(30, 51, 2, 1.6, 0, fill="k"),
    ]
    return Tree(canopy, trunk, Face(32, 24, 0.95, 0.85), (32, 39))


def baobab():
    canopy = [
        ellipse(32, 23, 16, 5.5),
        line([(26, 25), (22, 17)], "w", 2.4),
        line([(32, 25), (32, 15)], "w", 2.6),
        line([(38, 25), (43, 17)], "w", 2.4),
    ]
    trunk = [round_poly([(20, 54), (44, 54), (39, 26), (25, 26)], 3)]
    return Tree(canopy, trunk, Face(32, 40, 0.95, 0.9), (32, 28))


def sapling():
    canopy = [
        Circle(32, 29, 9.5),
    ]
    trunk = [
        line([(32, 54), (32, 37)], "w", 2.6),
        ellipse(37.5, 43.5, 5.2, 2.4, 35),
        half_disc(32, 54, 9, 3, lower=False),
    ]
    return Tree(canopy, trunk, Face(32, 29, 0.8), (32, 39))


def maple():
    pts = []
    cx, cy = 32, 26
    for k in range(10):
        a = math.radians(-90 + k * 36)
        rad = 19 if k % 2 == 0 else 11.5
        pts.append((cx + rad * math.cos(a), cy + rad * math.sin(a)))
    canopy = [round_poly(pts, 2.2)]
    trunk = [Rect(29.5, 38, 5, 16, 1.5)]
    return Tree(canopy, trunk, Face(32, 27, 0.95), (32, 42))


def acacia():
    canopy = [
        half_disc(32, 20, 20, 12, lower=True),
        Rect(12, 18, 40, 3.5, 1.75),
    ]
    trunk = [
        line([(32, 54), (32, 30)], "w", 3.4),
        line([(32, 40), (24, 30)], "w", 2.4),
        line([(32, 40), (41, 30)], "w", 2.4),
    ]
    return Tree(canopy, trunk, Face(32, 25, 0.8, 1.1), (32, 32))


def spruce():
    canopy = [
        cone((32, 5), 7, 16, 1.2),
        cone((32, 10), 10.5, 24, 1.5),
        cone((32, 16), 14, 33, 1.8),
        cone((32, 23), 17.5, 43, 2.0),
    ]
    trunk = [Rect(29.5, 42, 5, 12, 1.5)]
    return Tree(canopy, trunk, Face(32, 34, 0.9, 0.85), (32, 43))


def cherry():
    canopy = [
        Circle(32, 19, 9),
        Circle(22, 24, 8),
        Circle(42, 24, 8),
        Circle(32, 26, 9),
        curve((19, 30), (16, 42), -1.2, "w", 1.8),
        curve((26, 34), (24, 45), -0.8, "w", 1.8),
        curve((38, 34), (40, 45), 0.8, "w", 1.8),
        curve((45, 30), (48, 42), 1.2, "w", 1.8),
        Circle(16, 42, 2),
        Circle(24, 45, 2),
        Circle(40, 45, 2),
        Circle(48, 42, 2),
    ]
    trunk = [Rect(30, 34, 4, 20, 1.5)]
    return Tree(canopy, trunk, Face(32, 24, 0.9), (32, 36))


def cactus():
    canopy = [
        Rect(26.5, 12, 11, 42, 5.5),
        Rect(14, 22, 6, 15, 3),
        Rect(14, 31, 15, 6, 3),
        Rect(44, 17, 6, 17, 3),
        Rect(35, 28, 15, 6, 3),
    ]
    trunk = []
    return Tree(canopy, trunk, Face(32, 26, 0.9, 0.8), (32, 54))


def sequoia():
    canopy = [
        cone((32, 4), 5, 13, 1.4),
        cone((32, 9), 7.5, 20, 1.7),
        cone((32, 15), 10, 28, 2.0),
        cone((32, 22), 12.5, 37, 2.2),
        cone((32, 29), 14.5, 45, 2.4),
    ]
    trunk = [round_poly([(26.5, 43), (37.5, 43), (40.5, 54), (23.5, 54)], 1.5)]
    return Tree(canopy, trunk, Face(32, 35, 0.85, 0.8), (32, 45))


def olive():
    canopy = [
        Circle(22, 26, 8.5),
        Circle(42, 22, 9),
        Circle(33, 30, 9.5),
        Circle(30, 16, 7),
        Circle(47, 32, 5.5),
        Circle(17, 17, 4),
    ]
    trunk = [
        curve((31, 54), (30, 33), -2.5, "w", 4.5),
        line([(31, 45), (39, 35)], "w", 3),
    ]
    return Tree(canopy, trunk, Face(34, 27, 0.95, 0.9), (32, 38))


def ginkgo():
    canopy = [
        blob(
            [(32, 38), (21, 31), (10, 20), (12.5, 13), (22, 10), (32, 10), (42, 10), (51.5, 13), (54, 20), (43, 31)],
            0.6,
        ),
    ]
    trunk = [Rect(30.5, 37, 3, 17, 1.5)]
    return Tree(
        canopy,
        trunk,
        Face(32, 25, 0.9),
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
