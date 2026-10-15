#!/usr/bin/env node
// Renders the tree avatar Lottie files with lottie-web (canvas renderer) in
// headless Chromium. This is also the lottie-web compatibility test: a file
// that fails to load or draw here fails the build.
//
//   npm install lottie-web puppeteer-core          (in any scratch dir)
//   NODE_PATH=<scratch>/node_modules node scripts/avatars/render.js strips  [slug,...]
//   NODE_PATH=<scratch>/node_modules node scripts/avatars/render.js frames  <outdir> <px> [slug,...]
//
// `strips` writes assets/avatars/trees/frames/<slug>-<state>[@2x|-rail|-rail@2x].png:
//   a horizontal strip of square frames sampled at 30 fps, transparent outside
//   the disc. `frames` dumps every 60 fps frame of each state as numbered PNGs
//   over a dark background, for ffmpeg (previews).

const fs = require("fs");
const path = require("path");
const puppeteer = require("puppeteer-core");

const REPO = path.resolve(__dirname, "..", "..");
const ROOT = path.join(REPO, "assets", "avatars", "trees");
const manifest = JSON.parse(fs.readFileSync(path.join(ROOT, "manifest.json"), "utf8"));
const STATES = Object.keys(manifest.states);
const STRIP_SIZES = manifest.frame_strips.sizes; // {"1x":28,"2x":56,"rail":72,"rail@2x":144}
const STRIP_FPS = manifest.frame_strips.fps;
const SUFFIX = { "1x": "", "2x": "@2x", rail: "-rail", "rail@2x": "-rail@2x" };
const CHROME = process.env.CHROME || "/usr/local/bin/google-chrome";
const lottieSrc = fs.readFileSync(require.resolve("lottie-web/build/player/lottie_canvas.min.js"), "utf8");

function readySlugs(filter) {
  return manifest.characters
    .filter((c) => c.ready && (!filter || filter.includes(c.slug)))
    .map((c) => c.slug);
}

async function newPage(browser) {
  const page = await browser.newPage();
  await page.setViewport({ width: 400, height: 400, deviceScaleFactor: 1 });
  await page.setContent('<!doctype html><html><body style="margin:0;background:transparent"></body></html>');
  await page.addScriptTag({ content: lottieSrc });
  await page.evaluate(() => {
    // Renders `data` at `px` and returns one base64 PNG per requested frame,
    // composed either as a horizontal strip or as separate images.
    window.renderFrames = (data, px, frames, mode, bg) => {
      const holder = document.createElement("div");
      holder.style.cssText = `width:${px}px;height:${px}px;position:absolute;left:0;top:0`;
      document.body.appendChild(holder);
      const anim = lottie.loadAnimation({
        container: holder,
        renderer: "canvas",
        loop: false,
        autoplay: false,
        animationData: data,
        rendererSettings: { clearCanvas: true, preserveAspectRatio: "xMidYMid meet" },
      });
      const src = holder.querySelector("canvas");
      let out;
      if (mode === "strip") {
        const strip = document.createElement("canvas");
        strip.width = px * frames.length;
        strip.height = px;
        const ctx = strip.getContext("2d");
        frames.forEach((f, k) => {
          anim.goToAndStop(f, true);
          ctx.drawImage(src, k * px, 0, px, px);
        });
        out = [strip.toDataURL("image/png")];
      } else {
        out = frames.map((f) => {
          anim.goToAndStop(f, true);
          const c = document.createElement("canvas");
          c.width = px;
          c.height = px;
          const ctx = c.getContext("2d");
          if (bg) {
            ctx.fillStyle = bg;
            ctx.fillRect(0, 0, px, px);
          }
          ctx.drawImage(src, 0, 0, px, px);
          return c.toDataURL("image/png");
        });
      }
      anim.destroy();
      holder.remove();
      return out;
    };
  });
  page.on("pageerror", (e) => {
    throw e;
  });
  return page;
}

function savePng(dataUrl, file) {
  fs.writeFileSync(file, Buffer.from(dataUrl.split(",")[1], "base64"));
}

async function strips(page, slugs) {
  const outDir = path.join(ROOT, "frames");
  fs.mkdirSync(outDir, { recursive: true });
  for (const slug of slugs) {
    for (const state of STATES) {
      const data = JSON.parse(fs.readFileSync(path.join(ROOT, "lottie", slug, `${state}.json`), "utf8"));
      const step = data.fr / STRIP_FPS;
      const count = Math.round(data.op / step);
      const frames = Array.from({ length: count }, (_, k) => k * step);
      for (const [key, px] of Object.entries(STRIP_SIZES)) {
        const [png] = await page.evaluate((...a) => window.renderFrames(...a), data, px, frames, "strip", null);
        savePng(png, path.join(outDir, `${slug}-${state}${SUFFIX[key]}.png`));
      }
      console.log(`${slug} ${state}: ${count} frames x ${Object.values(STRIP_SIZES).join("/")} px`);
    }
  }
}

async function frames(page, outRoot, px, slugs) {
  for (const slug of slugs) {
    for (const state of STATES) {
      const data = JSON.parse(fs.readFileSync(path.join(ROOT, "lottie", slug, `${state}.json`), "utf8"));
      const dir = path.join(outRoot, `${slug}-${state}`);
      fs.mkdirSync(dir, { recursive: true });
      const all = Array.from({ length: data.op }, (_, k) => k);
      const pngs = await page.evaluate((...a) => window.renderFrames(...a), data, px, all, "frames", "#111111");
      pngs.forEach((p, k) => savePng(p, path.join(dir, `${String(k).padStart(4, "0")}.png`)));
      console.log(`${slug} ${state}: ${pngs.length} frames at ${px} px -> ${dir}`);
    }
  }
}

async function main() {
  const [mode, ...rest] = process.argv.slice(2);
  const browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: true,
    args: ["--no-sandbox", "--disable-dev-shm-usage", "--hide-scrollbars"],
  });
  try {
    const page = await newPage(browser);
    if (mode === "strips") {
      await strips(page, readySlugs(rest[0] ? rest[0].split(",") : null));
    } else if (mode === "frames") {
      const [outRoot, px, only] = rest;
      await frames(page, outRoot, Number(px), readySlugs(only ? only.split(",") : null));
    } else {
      throw new Error("usage: render.js strips [slugs] | frames <outdir> <px> [slugs]");
    }
  } finally {
    await browser.close();
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
