# Arbos leaf avatars v1

Nine cream leaf silhouettes for agent avatars. The app draws the coloured circle; these files are the leaf only.

## Files
- `manifest.json` lists every leaf (id, name, SVG, idle/gust Lottie, cream PNG, default circle colour, stem pivot) plus the 10-colour palette. Treat it as the source of truth.
- `svg/leaf-NN-name.svg` has one `currentColor` path (even-odd) in a 100×100 viewBox. The leaf is centred and spans 62% of the circle.
- `lottie/leaf-NN-name-idle.json` is a bodymovin v5 file: 100×100, 60 fps, a seamless 2.3–2.8 s wind-sway loop. It rotates a few degrees about the stem base with a slight skew/scale ruffle. Frame 0 is the rest pose and equals the last frame. Loop lengths differ per leaf so a list of avatars doesn't sway in unison.
- `lottie/leaf-NN-name-gust.json` is a one-shot hover gust (60 frames = 1 s). A spring-damper swings it to about 9°, then it settles exactly back to the rest pose, so it hands off cleanly to idle frame 0.
- `lottie/frames.jpg` and `lottie/preview.mp4` are render checks (lottie-web): each leaf on its default circle, the idle loops in a 3×3 grid, and a staggered gust on each.
- `png/` holds cream transparent 1024 px PNGs, plus 512 px PNGs of each leaf on each of the 10 palette colours (90 files).
- `nine.jpg` and `compare.jpg` are design overview sheets.

## Lottie details
- Each file has one shape layer `leaf` → group `shape` → fill `leaf` (#F6F0E8, even-odd). To recolour, use keypath `**.leaf.Color` (lottie-ios `ColorValueProvider`, lottie-android `KeyPath("**","leaf")` + `LottieProperty.COLOR`). The background is transparent and there is no circle.
- Honour reduced motion: show the static SVG/frame 0 and skip idle and gust.

## Default leaf + colour
The default leaf and circle colour are **deterministic from the agent id, computed on the app side**. Hash the agent id with a stable hash (do not use a per-process randomised hash). Then use `leaf = leaves[h % 9]`. For the colour, use that leaf's `default_color`, or `palette[(h / 9) % 10]` if you want more variety. The same agent always gets the same avatar on every device. Users may override it.

## Palette
Mustard #BCA368 · Olive #8D9659 · Moss #606440 · Sage #608F80 · Pine #416A5B · Dusty blue #396788 · Mauve #A56489 · Rose #B24E61 · Coral #CC6B6D · Terracotta #C17269. Leaf cream #F6F0E8.
