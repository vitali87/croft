// Replay captured ANSI frames into xterm.js in headless Chromium and
// screenshot each one. Called by tui_demo.py:
//   node render.mjs <frames dir> <cols>x<rows> <fonts dir>
// reads <frames dir>/index.json and NNNN.ans, writes NNNN.png beside them.
import { chromium } from "playwright-core";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [framesDir, size, fontsDir] = process.argv.slice(2);
const [cols, rows] = size.split("x").map(Number);
const here = path.dirname(new URL(import.meta.url).pathname);
const frames = JSON.parse(fs.readFileSync(path.join(framesDir, "index.json")));
const pkg = (p) => fs.readFileSync(path.join(here, "node_modules/@xterm/xterm", p), "utf8");
const font = (f) => fs.readFileSync(path.join(fontsDir, f)).toString("base64");

// playwright-core wants the Chromium build matching its own version. Take an
// explicit one, else that one, else any Chromium playwright installed.
function chromiumPath() {
  if (process.env.CROFT_DEMO_CHROMIUM) return process.env.CROFT_DEMO_CHROMIUM;
  if (fs.existsSync(chromium.executablePath())) return chromium.executablePath();
  const roots = [process.env.PLAYWRIGHT_BROWSERS_PATH, path.join(os.homedir(), ".cache/ms-playwright")];
  for (const root of roots.filter(Boolean)) {
    if (!fs.existsSync(root)) continue;
    for (const d of fs.readdirSync(root).filter((d) => /^chromium-\d+$/.test(d)).sort().reverse()) {
      for (const bin of ["chrome-linux64/chrome", "chrome-linux/chrome", "chrome-mac/Chromium.app/Contents/MacOS/Chromium"]) {
        const p = path.join(root, d, bin);
        if (fs.existsSync(p)) return p;
      }
    }
  }
  throw new Error("no Chromium found: run `npx playwright install chromium` or set CROFT_DEMO_CHROMIUM");
}

const browser = await chromium.launch({ executablePath: chromiumPath() });
const page = await browser.newPage({ deviceScaleFactor: 1 });
await page.setContent(`<!doctype html><html><head><style>
  @font-face { font-family: NF; font-weight: normal; src: url(data:font/ttf;base64,${font("DejaVuSansMNerdFontMono-Regular.ttf")}); }
  @font-face { font-family: NF; font-weight: bold; src: url(data:font/ttf;base64,${font("DejaVuSansMNerdFontMono-Bold.ttf")}); }
  ${pkg("css/xterm.css")}
  body { margin: 0; background: #1e1e1e; }
  #t { display: inline-block; padding: 10px; background: #1e1e1e; }
</style></head><body><div id="t"></div><script>${pkg("lib/xterm.js")}</script></body></html>`);
await page.evaluate(async () => {
  await document.fonts.load("14px NF");
  await document.fonts.load("bold 14px NF");
});
await page.evaluate(({ cols, rows }) => {
  window.term = new Terminal({
    cols, rows, fontFamily: "NF, monospace", fontSize: 14, lineHeight: 1.1,
    cursorBlink: false, cursorInactiveStyle: "none",
    theme: { background: "#1e1e1e", foreground: "#d4d4d4" },
  });
  window.term.open(document.getElementById("t"));
}, { cols, rows });
const el = await page.$("#t");
for (const f of frames) {
  const text = fs.readFileSync(path.join(framesDir, f.file), "utf8").replace(/\n$/, "");
  await page.evaluate((t) => new Promise((done) => {
    window.term.reset();
    // Hide xterm's own cursor: croft draws its caret as a cell.
    window.term.write("\x1b[?25l" + t.split("\n").join("\r\n"), done);
  }), text);
  await el.screenshot({ path: path.join(framesDir, f.file.replace(".ans", ".png")) });
}
await browser.close();
