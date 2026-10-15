"""The 20 tree characters, in relay order.

The relay picks a default avatar with `slugs[fnv1a64(project_id) % n]` over
this list, so the ORDER IS PART OF THE CONTRACT. Append only; never reorder.
"""

# slug, display name, personality tag, default face
CHARACTERS = [
    ("oak", "Oakley", "steady", "calm"),
    ("pine", "Pip", "focused", "focused"),
    ("lollipop", "Lolli", "cheerful", "happy"),
    ("fractal", "Frax", "curious", "surprised"),
    ("mushroom", "Morel", "playful", "winking"),
    ("cypress", "Cy", "determined", "determined"),
    ("willow", "Willa", "gentle", "calm"),
    ("palm", "Palmer", "laid-back", "winking"),
    ("bonsai", "Bon", "patient", "calm"),
    ("birch", "Birdie", "bright", "happy"),
    ("baobab", "Bao", "wise", "calm"),
    ("sapling", "Sprout", "eager", "surprised"),
    ("maple", "Mabel", "warm", "happy"),
    ("acacia", "Ace", "sharp", "focused"),
    ("spruce", "Bruce", "dependable", "determined"),
    ("cherry", "Blossom", "dreamy", "winking"),
    ("cactus", "Spike", "tough", "determined"),
    ("sequoia", "Red", "grounded", "calm"),
    ("olive", "Ollie", "peaceful", "happy"),
    ("ginkgo", "Ko", "thoughtful", "focused"),
]

SLUGS = [c[0] for c in CHARACTERS]

FACES = ("happy", "focused", "winking", "surprised", "calm", "determined")
