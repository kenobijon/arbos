"""Build the arbos.life social preview (1200x630) with coloured v2 tree avatars (D74).

Reads assets/avatars/trees/<slug>.svg, recolours them per manifest colors[], lays them out in an
HTML page and screenshots it with headless Chrome (google-chrome or chromium on PATH).
Output: site/assets/og/arbos-social.png. Run: python scripts/site/build-og.py
"""
import json
import pathlib
import re
import shutil
import subprocess
import tempfile

REPO = pathlib.Path(__file__).resolve().parents[2]
TREES = REPO / "assets/avatars/trees"
OUT = REPO / "site/assets/og/arbos-social.png"
HTML = pathlib.Path(tempfile.mkdtemp()) / "og.html"

manifest = json.loads((TREES / "manifest.json").read_text())
colors = {c["slug"]: c for c in manifest["colors"]}
ROLES = ["bg", "ring", "tree", "tree_dark", "tree_light", "face"]

# (slug, color) — the approved trio first, then a spread of the palette.
CAST = [
    ("oak", "green"), ("fractal", "blue"), ("mushroom", "red"),
    ("pine", "amber"), ("cherry", "pink"), ("willow", "teal"),
    ("sequoia", "violet"), ("palm", "orange"), ("cactus", "cyan"),
]


def tint(svg: str, color: str) -> str:
    c = colors[color]
    def fix(m):
        tag = m.group(0)
        role = re.search(r'class="([a-z_]+)"', tag).group(1)
        hexv = c.get(role)
        if role == "ring":
            hexv = c.get("ring", "#ffffff")
        if not hexv:
            return tag
        tag = re.sub(r'fill="(?!none)[^"]*"', f'fill="{hexv}"', tag)
        tag = re.sub(r'stroke="(?!none)[^"]*"', f'stroke="{hexv}"', tag)
        return tag
    return re.sub(r"<[a-z]+ [^>]*class=\"[a-z_]+\"[^>]*>", fix, svg)


avatars = "".join(
    f'<div class="av">{tint((TREES / f"{slug}.svg").read_text(), color)}</div>' for slug, color in CAST
)
wordmark = (REPO / "site/assets/logo/arbos-wordmark-white.svg").read_text()
font = (REPO / "site/assets/fonts/Geist-Variable.woff2").resolve()

HTML.write_text(f"""<!doctype html><html><head><meta charset="utf-8">
<style>
@font-face {{ font-family: Geist; src: url("file://{font}") format("woff2"); font-weight: 100 900; }}
html,body {{ margin:0; width:1200px; height:630px; background:#0a0a0a; color:#fff; font-family: Geist, system-ui, sans-serif; overflow:hidden; }}
.page {{ position:relative; width:1200px; height:630px; display:flex; align-items:center; justify-content:space-between; padding:0 84px; box-sizing:border-box;
  background: radial-gradient(900px 500px at 85% 50%, rgba(255,255,255,0.06), transparent 60%), #0a0a0a; }}
.copy {{ max-width: 560px; }}
.copy svg {{ height: 56px; width:auto; display:block; margin-bottom: 34px; }}
h1 {{ font-size: 52px; line-height: 1.06; font-weight: 500; letter-spacing: -0.015em; margin: 0 0 22px; }}
p {{ font-size: 24px; line-height: 1.35; color: #c9cdd6; margin: 0; font-weight: 400; }}
.url {{ margin-top: 40px; font-size: 20px; color: #8b8f99; letter-spacing: 0.01em; }}
.cast {{ display:grid; grid-template-columns: repeat(3, 132px); gap: 22px; transform: rotate(-6deg); }}
.av {{ width:132px; height:132px; border-radius:50%; box-shadow: 0 18px 40px rgba(0,0,0,0.55); }}
.av svg {{ width:100%; height:100%; display:block; }}
</style></head><body><div class="page">
<div class="copy">{wordmark}
<h1>An always-on agent for every project.</h1>
<p>Chat from your Mac and iPhone. Your agents work on your own cloud machine and report back in prose.</p>
<div class="url">arbos.life</div></div>
<div class="cast">{avatars}</div>
</div></body></html>""")

OUT.parent.mkdir(parents=True, exist_ok=True)
chrome = next((c for c in ("google-chrome", "chromium", "chromium-browser") if shutil.which(c)), None)
if not chrome:
    raise SystemExit("needs google-chrome or chromium on PATH")
subprocess.run([
    chrome, "--headless=new", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
    "--allow-file-access-from-files", "--window-size=1200,630", f"--screenshot={OUT}", f"file://{HTML}",
], check=True, capture_output=True)
print(OUT, OUT.stat().st_size)
