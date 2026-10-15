"""Flat-vector primitives in the 64x64 avatar space.

Every primitive exports itself both as SVG markup and as a Lottie shape
item, so the still SVG and the animated Lottie are built from one geometry.
Paint is either a fill (`fill="w"|"k"`) or a stroke (`stroke=("w"|"k", width)`).
"""

import math

WHITE = "#ffffff"
BLACK = "#000000"
KAPPA = 0.5522847498

_COLOR = {"w": WHITE, "k": BLACK}
_RGB = {"w": [1, 1, 1, 1], "k": [0, 0, 0, 1]}


def r2(x):
    v = round(float(x), 2)
    return int(v) if v == int(v) else v


def fmt(x):
    return repr(r2(x))


# Recolor roles. Every paint carries one of these names (SVG `class`, Lottie
# `nm`) so apps can retint by role: bg = disc, ring = outline, tree = white
# ink, face = the dark ink (eyes, mouth, bark marks), which takes the bg color.
ROLE_OF_COLOR = {"w": "tree", "k": "face"}


class Prim:
    fill = None
    stroke = None
    role = None

    def paint_key(self):
        return (("f", self.fill) if self.fill else ("s",) + tuple(self.stroke)) + (self.role,)

    def paint_role(self):
        return self.role or ROLE_OF_COLOR[self.fill or self.stroke[0]]

    def svg_paint(self):
        cls = f'class="{self.paint_role()}" '
        if self.fill:
            return f'{cls}fill="{_COLOR[self.fill]}"'
        c, w = self.stroke
        return (
            f'{cls}fill="none" stroke="{_COLOR[c]}" stroke-width="{fmt(w)}" '
            'stroke-linecap="round" stroke-linejoin="round"'
        )

    def lottie_paint(self):
        nm = self.paint_role()
        if self.fill:
            return {"ty": "fl", "nm": nm, "c": {"a": 0, "k": _RGB[self.fill]}, "o": {"a": 0, "k": 100}, "r": 1}
        c, w = self.stroke
        return {
            "ty": "st",
            "nm": nm,
            "c": {"a": 0, "k": _RGB[c]},
            "o": {"a": 0, "k": 100},
            "w": {"a": 0, "k": r2(w)},
            "lc": 2,
            "lj": 2,
        }


class Circle(Prim):
    def __init__(self, cx, cy, r, fill="w", stroke=None, role=None):
        self.cx, self.cy, self.r = cx, cy, r
        self.fill, self.stroke = (None if stroke else fill), stroke
        self.role = role

    def svg(self):
        return f'<circle cx="{fmt(self.cx)}" cy="{fmt(self.cy)}" r="{fmt(self.r)}" {self.svg_paint()}/>'

    def lottie(self):
        d = 2 * self.r
        return {"ty": "el", "p": {"a": 0, "k": [r2(self.cx), r2(self.cy)]}, "s": {"a": 0, "k": [r2(d), r2(d)]}}


class Rect(Prim):
    def __init__(self, x, y, w, h, r=0, fill="w"):
        self.x, self.y, self.w, self.h, self.r = x, y, w, h, r
        self.fill = fill

    def svg(self):
        rx = f' rx="{fmt(self.r)}"' if self.r else ""
        return (
            f'<rect x="{fmt(self.x)}" y="{fmt(self.y)}" width="{fmt(self.w)}" '
            f'height="{fmt(self.h)}"{rx} {self.svg_paint()}/>'
        )

    def lottie(self):
        return {
            "ty": "rc",
            "p": {"a": 0, "k": [r2(self.x + self.w / 2), r2(self.y + self.h / 2)]},
            "s": {"a": 0, "k": [r2(self.w), r2(self.h)]},
            "r": {"a": 0, "k": r2(self.r)},
        }


class Path(Prim):
    """Cubic bezier path. `pts` is a list of (vertex, in_tangent, out_tangent);
    tangents are relative to the vertex, Lottie style."""

    def __init__(self, pts, closed=True, fill="w", stroke=None):
        self.pts = pts
        self.closed = closed
        self.fill, self.stroke = (None if stroke else fill), stroke

    def svg(self):
        pts = self.pts
        d = [f"M{fmt(pts[0][0][0])} {fmt(pts[0][0][1])}"]
        n = len(pts)
        segs = n if self.closed else n - 1
        for k in range(segs):
            (v0, _, o0), (v1, i1, _) = pts[k], pts[(k + 1) % n]
            c1 = (v0[0] + o0[0], v0[1] + o0[1])
            c2 = (v1[0] + i1[0], v1[1] + i1[1])
            if o0 == (0, 0) and i1 == (0, 0):
                d.append(f"L{fmt(v1[0])} {fmt(v1[1])}")
            else:
                d.append(
                    f"C{fmt(c1[0])} {fmt(c1[1])} {fmt(c2[0])} {fmt(c2[1])} {fmt(v1[0])} {fmt(v1[1])}"
                )
        if self.closed:
            d.append("Z")
        return f'<path d="{"".join(d)}" {self.svg_paint()}/>'

    def lottie(self):
        return {
            "ty": "sh",
            "ks": {
                "a": 0,
                "k": {
                    "c": self.closed,
                    "v": [[r2(v[0]), r2(v[1])] for v, _, _ in self.pts],
                    "i": [[r2(i[0]), r2(i[1])] for _, i, _ in self.pts],
                    "o": [[r2(o[0]), r2(o[1])] for _, _, o in self.pts],
                },
            },
        }


# ---------- path builders ----------


def polygon(points, fill="w", stroke=None, closed=True):
    return Path([(p, (0, 0), (0, 0)) for p in points], closed=closed, fill=fill, stroke=stroke)


def line(points, color="w", width=2):
    """Open polyline stroke."""
    return polygon(points, stroke=(color, width), closed=False)


def curve(p0, p1, bulge, color="w", width=2):
    """Open stroke from p0 to p1 bowing sideways by `bulge` (positive = to the
    right of travel, i.e. downward for a left-to-right stroke)."""
    dx, dy = p1[0] - p0[0], p1[1] - p0[1]
    length = math.hypot(dx, dy) or 1
    nx, ny = -dy / length, dx / length
    # Cubic with both controls offset gives a bulge of 0.75*ctrl at the middle.
    c = bulge / 0.75
    t = (dx / 3 + nx * c, dy / 3 + ny * c)
    return Path(
        [(p0, (0, 0), t), (p1, (-dx / 3 + nx * c, -dy / 3 + ny * c), (0, 0))],
        closed=False,
        stroke=(color, width),
    )


def ellipse(cx, cy, rx, ry, rot=0, fill="w"):
    """Ellipse as a 4-segment bezier (lets us rotate it everywhere)."""
    a = math.radians(rot)
    ca, sa = math.cos(a), math.sin(a)

    def tr(x, y):
        return (cx + x * ca - y * sa, cy + x * sa + y * ca)

    def tv(x, y):
        return (x * ca - y * sa, x * sa + y * ca)

    kx, ky = KAPPA * rx, KAPPA * ry
    pts = [
        (tr(0, -ry), tv(-kx, 0), tv(kx, 0)),
        (tr(rx, 0), tv(0, -ky), tv(0, ky)),
        (tr(0, ry), tv(kx, 0), tv(-kx, 0)),
        (tr(-rx, 0), tv(0, ky), tv(0, -ky)),
    ]
    return Path(pts, fill=fill)


def half_disc(cx, cy, rx, ry, lower=True, fill="w"):
    """Flat side on top (lower=True) or bottom."""
    s = 1 if lower else -1
    kx, ky = KAPPA * rx, KAPPA * ry
    pts = [
        ((cx - rx, cy), (0, 0), (0, s * ky)),
        ((cx, cy + s * ry), (-kx, 0), (kx, 0)),
        ((cx + rx, cy), (0, s * ky), (0, 0)),
    ]
    return Path(pts, fill=fill)


def blob(points, tension=1.0, fill="w"):
    """Closed smooth shape through `points` (Catmull-Rom to bezier)."""
    n = len(points)
    pts = []
    for k in range(n):
        p_prev, p_next = points[(k - 1) % n], points[(k + 1) % n]
        tx = (p_next[0] - p_prev[0]) / 6 * tension
        ty = (p_next[1] - p_prev[1]) / 6 * tension
        pts.append((points[k], (-tx, -ty), (tx, ty)))
    return Path(pts, fill=fill)


def cone(tip, half_width, base_y, sag=0.0, fill="w"):
    """Triangle with a tip, optionally with a drooping (curved) base."""
    tx, ty = tip
    left, right = (tx - half_width, base_y), (tx + half_width, base_y)
    if sag == 0:
        return polygon([tip, right, left], fill=fill)
    c = sag / 0.75
    return Path(
        [
            (tip, (0, 0), (0, 0)),
            (right, (0, 0), (-half_width * 2 / 3, c)),
            (left, (half_width * 2 / 3, c), (0, 0)),
        ],
        fill=fill,
    )


def round_poly(points, radius, fill="w"):
    """Polygon whose corners are rounded with `radius` (via bezier corners)."""
    n = len(points)
    pts = []
    for k in range(n):
        p_prev, p, p_next = points[(k - 1) % n], points[k], points[(k + 1) % n]
        d1 = (p_prev[0] - p[0], p_prev[1] - p[1])
        d2 = (p_next[0] - p[0], p_next[1] - p[1])
        l1, l2 = math.hypot(*d1) or 1, math.hypot(*d2) or 1
        rr = min(radius, l1 / 2.2, l2 / 2.2)
        a = (p[0] + d1[0] / l1 * rr, p[1] + d1[1] / l1 * rr)
        b = (p[0] + d2[0] / l2 * rr, p[1] + d2[1] / l2 * rr)
        k_ = KAPPA * rr
        pts.append((a, (0, 0), (-d1[0] / l1 * k_, -d1[1] / l1 * k_)))
        pts.append((b, (-d2[0] / l2 * k_, -d2[1] / l2 * k_), (0, 0)))
    return Path(pts, fill=fill)
