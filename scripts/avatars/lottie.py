"""Lottie JSON export for the three animation states of a tree character.

Only features that lottie-web and lottie-ios share are used: shape layers
with ellipse / rect / path items, fills and strokes, group transforms with
bezier or hold keyframes. No expressions, masks, mattes, merge paths, trim
paths, or raster assets.

Layer stack (top to bottom): ring (white 2 px stroke), tree (one shape
layer), disc (black circle). The ring sits above the tree so a bounce or sway
that reaches the rim slides under it instead of breaking the circle. Inside the tree layer, groups are listed top-first, as Lottie
draws them:

    sparkles (working only)
    canopy  [scale = breathing / pulse, pivot at the canopy base]
        face
            eyes        [scale y = blink, position = look-around]
            brows
            mouth       [opacity]
            mouth_reply [opacity]
        canopy black detail
        canopy white shapes
    trunk

The whole tree layer rotates around the trunk base for the sway and moves /
squashes for the reply bounce.
"""

from itertools import groupby

from shapes import Circle, r2
from trees import SPARKLES, SWAY_PIVOT, face_parts

FPS = 60
DURATION = {"idle": 180, "working": 180, "reply": 60}
LOOPS = {"idle": True, "working": True, "reply": False}

# Easing presets (After Effects style, x = time, y = value).
EASE_SINE = ({"x": 0.37, "y": 0}, {"x": 0.63, "y": 1})
EASE_OUT = ({"x": 0.2, "y": 0}, {"x": 0.3, "y": 1})
EASE_IN_OUT = ({"x": 0.42, "y": 0}, {"x": 0.58, "y": 1})


def static(v):
    return {"a": 0, "k": v}


def animated(keys, ease=EASE_SINE):
    """keys: list of (frame, value) or (frame, value, 'hold')."""
    out = []
    dims = len(keys[0][1]) if isinstance(keys[0][1], list) else 1
    for n, key in enumerate(keys):
        t, v = key[0], key[1]
        kf = {"t": t, "s": v if isinstance(v, list) else [v]}
        if n < len(keys) - 1:
            if len(key) > 2 and key[2] == "hold":
                kf["h"] = 1
            else:
                o, i = ease
                kf["o"] = {"x": [o["x"]] * dims, "y": [o["y"]] * dims}
                kf["i"] = {"x": [i["x"]] * dims, "y": [i["y"]] * dims}
        out.append(kf)
    return {"a": 1, "k": out}


def transform(anchor=(0, 0), position=None, scale=None, rotation=None, opacity=None):
    position = position if position is not None else anchor
    return {
        "ty": "tr",
        "a": static([r2(anchor[0]), r2(anchor[1])]),
        "p": position if isinstance(position, dict) else static([r2(position[0]), r2(position[1])]),
        "s": scale if isinstance(scale, dict) else static(list(scale) if scale else [100, 100]),
        "r": rotation if isinstance(rotation, dict) else static(rotation or 0),
        "o": opacity if isinstance(opacity, dict) else static(100 if opacity is None else opacity),
    }


def paint_groups(prims):
    """Prims that share a paint collapse into one group: [shapes..., paint, tr]."""
    groups = []
    for _, run in groupby(prims, key=lambda p: p.paint_key()):
        run = list(run)
        groups.append(
            {
                "ty": "gr",
                "it": [p.lottie() for p in run] + [run[0].lottie_paint(), transform()],
            }
        )
    return groups


def group(name, items, tr=None):
    return {"ty": "gr", "nm": name, "it": items + [tr or transform()]}


def layer(name, index, items, ks, op):
    return {
        "ddd": 0,
        "ind": index,
        "ty": 4,
        "nm": name,
        "sr": 1,
        "ks": ks,
        "ao": 0,
        "shapes": items,
        "ip": 0,
        "op": op,
        "st": 0,
        "bm": 0,
    }


def layer_ks(anchor, position=None, scale=None, rotation=None):
    return {
        "o": static(100),
        "r": rotation if isinstance(rotation, dict) else static(rotation or 0),
        "p": position if isinstance(position, dict) else static([r2(anchor[0]), r2(anchor[1])]),
        "a": static([r2(anchor[0]), r2(anchor[1])]),
        "s": scale if isinstance(scale, dict) else static([100, 100]),
    }


def ring_items():
    return [group("ring", paint_groups([Circle(32, 32, 31, stroke=("w", 2))]))]


def disc_items():
    return [group("disc", paint_groups([Circle(32, 32, 32, fill="k")]))]


# ---------- per-state motion ----------


def motion(state, tree, face):
    """Returns the animated properties for one state."""
    n = DURATION[state]
    ex, ey = face["eye_center"]
    m = {
        "layer_rot": None,
        "layer_pos": None,
        "layer_scale": None,
        "canopy_scale": None,
        "eyes_scale": None,
        "eyes_pos": None,
        "mouth_o": None,
        "reply_o": static(0),
        "sparkles": None,
    }
    if state == "idle":
        m["layer_rot"] = animated([(0, -2.2), (n // 2, 2.2), (n, -2.2)])
        m["canopy_scale"] = animated(
            [(0, [100, 100]), (n // 4, [102.5, 102]), (3 * n // 4, [98.5, 98.8]), (n, [100, 100])]
        )
        b = 118  # one blink per loop, ~130 ms
        m["eyes_scale"] = animated(
            [(0, [100, 100]), (b, [100, 100], "hold"), (b + 4, [100, 10]), (b + 8, [100, 100]), (n, [100, 100], "hold")],
            EASE_IN_OUT,
        )
    elif state == "working":
        m["layer_rot"] = animated([(0, -1.2), (n // 2, 1.2), (n, -1.2)])
        pulse = []
        for k in range(3):
            t0 = k * 60
            pulse += [(t0, [100, 100]), (t0 + 30, [104.5, 104])]
        pulse.append((n, [100, 100]))
        m["canopy_scale"] = animated(pulse)
        look = 1.7 * tree.face.scale
        m["eyes_pos"] = animated(
            [
                (0, [ex, ey]),
                (18, [ex - look, ey - 0.4], "hold"),
                (62, [ex - look, ey - 0.4]),
                (80, [ex + look, ey - 0.4], "hold"),
                (124, [ex + look, ey - 0.4]),
                (142, [ex, ey - 0.8], "hold"),
                (166, [ex, ey - 0.8]),
                (n, [ex, ey]),
            ],
            EASE_IN_OUT,
        )
        m["eyes_scale"] = animated(
            [(0, [100, 100]), (70, [100, 100], "hold"), (74, [100, 10]), (78, [100, 100]), (n, [100, 100], "hold")],
            EASE_IN_OUT,
        )
        m["sparkles"] = [
            animated(
                [(0, [0, 0]), (t0, [0, 0], "hold"), (t0 + 14, [100, 100]), (t0 + 24, [100, 100], "hold"), (t0 + 38, [0, 0]), (n, [0, 0], "hold")],
                EASE_OUT,
            )
            for t0 in (2, 58, 114)
        ]
    elif state == "reply":
        px, py = SWAY_PIVOT
        m["layer_pos"] = animated(
            [(0, [px, py]), (6, [px, py + 1.2]), (16, [px, py - 3.2]), (28, [px, py]), (36, [px, py - 1.2]), (44, [px, py]), (n, [px, py], "hold")],
            EASE_IN_OUT,
        )
        m["layer_scale"] = animated(
            [(0, [100, 100]), (6, [106, 94]), (16, [96, 104]), (28, [103, 98]), (38, [100, 100]), (n, [100, 100], "hold")],
            EASE_IN_OUT,
        )
        m["layer_rot"] = animated([(0, 0), (14, -4), (30, 3), (44, 0), (n, 0, "hold")], EASE_IN_OUT)
        m["mouth_o"] = animated([(0, 100, "hold"), (8, 0, "hold"), (50, 100, "hold"), (n, 100)])
        m["reply_o"] = animated([(0, 0, "hold"), (8, 100, "hold"), (50, 0, "hold"), (n, 0)])
        m["eyes_scale"] = animated(
            [(0, [100, 100]), (8, [100, 100], "hold"), (12, [110, 35]), (46, [110, 35], "hold"), (50, [100, 100]), (n, [100, 100], "hold")],
            EASE_IN_OUT,
        )
    else:
        raise ValueError(state)
    return m


def tree_lottie(slug, name, face_kind, tree, state):
    face = face_parts(face_kind, tree.face)
    n = DURATION[state]
    m = motion(state, tree, face)
    ex, ey = face["eye_center"]

    face_items = [
        group(
            "eyes",
            paint_groups(face["eyes"]),
            transform((ex, ey), position=m["eyes_pos"], scale=m["eyes_scale"]),
        ),
    ]
    if face["brows"]:
        face_items.append(group("brows", paint_groups(face["brows"])))
    face_items.append(group("mouth", paint_groups(face["mouth"]), transform(opacity=m["mouth_o"])))
    face_items.append(group("mouth_reply", paint_groups(face["reply_mouth"]), transform(opacity=m["reply_o"])))

    canopy_items = [group("face", face_items)]
    canopy_items += paint_groups(tree.canopy_black)
    canopy_items += paint_groups(tree.canopy)
    cp = tree.canopy_pivot
    items = [group("canopy", canopy_items, transform(cp, scale=m["canopy_scale"]))]

    if m["sparkles"]:
        sp = []
        for ((sx, sy), sr), sc in zip(SPARKLES, m["sparkles"]):
            sp.append(group("sparkle", paint_groups([Circle(sx, sy, sr)]), transform((sx, sy), scale=sc)))
        items.insert(0, group("sparkles", sp))

    if tree.trunk:
        items.append(group("trunk", paint_groups(tree.trunk)))

    ring_layer = layer("ring", 1, ring_items(), layer_ks((32, 32)), n)
    tree_layer = layer(
        "tree",
        2,
        items,
        layer_ks(SWAY_PIVOT, position=m["layer_pos"], scale=m["layer_scale"], rotation=m["layer_rot"]),
        n,
    )
    disc_layer = layer("disc", 3, disc_items(), layer_ks((32, 32)), n)
    return {
        "v": "5.12.1",
        "fr": FPS,
        "ip": 0,
        "op": n,
        "w": 64,
        "h": 64,
        "nm": f"{name} {state}",
        "ddd": 0,
        "assets": [],
        "layers": [ring_layer, tree_layer, disc_layer],
        "markers": [],
        "meta": {"g": "arbos scripts/avatars", "a": "Arbos", "d": f"{slug} {state}; loop={LOOPS[state]}"},
    }
