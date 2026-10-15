"""Still SVG export of a tree character."""

from characters import CHARACTERS
from lottie import RING_OPACITY
from trees import TREES, face_parts

FACE_OF = {slug: face for slug, _, _, face in CHARACTERS}


def tree_svg(slug: str, name: str) -> str:
    tree = TREES[slug]()
    face = face_parts(FACE_OF[slug], tree.face)
    body = []
    body.append('<circle class="bg" cx="32" cy="32" r="32" fill="#000000"/>')
    for prim in tree.trunk:
        body.append(prim.svg())
    for prim in tree.canopy:
        body.append(prim.svg())
    for prim in tree.canopy_black:
        body.append(prim.svg())
    for key in ("eyes", "brows", "mouth"):
        for prim in face[key]:
            body.append(prim.svg())
    # The ring is drawn last, like the Lottie, so the tree sits under it.
    body.append(
        '<circle class="ring" cx="32" cy="32" r="31" fill="none" stroke="#ffffff" '
        f'stroke-opacity="{RING_OPACITY / 100:g}" stroke-width="2"/>'
    )
    inner = "\n".join(body)
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" role="img" aria-label="{name}">\n'
        f"{inner}\n</svg>\n"
    )
