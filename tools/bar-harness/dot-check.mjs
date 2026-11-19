// Renders the watching dot and the paused ring, with the page's own CSS, in headless
// Edge at several device scales, idle and mid-pulse, on light and dark backgrounds,
// and reports how far each is from a symmetric circle (the dot must be a clean circle
// in every state; the founder saw two stray light pixels at its top and left while idle).
//
// node tools/bar-harness/dot-check.mjs [--out target/dot-check] [--png montage.png]
//
// The CSS is read from src/routes/+page.svelte, so a change there is what is measured.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { decodePng, newTab, startEdge } from './lib.mjs';

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => {
    if (a.startsWith('--')) acc.push([a.slice(2), all[i + 1] && !all[i + 1].startsWith('--') ? all[i + 1] : 'true']);
    return acc;
  }, []),
);
const here = dirname(fileURLToPath(import.meta.url));
const out = resolve(args.out ?? 'target/dot-check');
mkdirSync(out, { recursive: true });
const svelte = readFileSync(join(here, '../../src/routes/+page.svelte'), 'utf8');
const style = svelte.slice(svelte.indexOf('<style'));
const grab = (re) => [...style.matchAll(re)].map((m) => m[0]).join('\n');
const css = [
  grab(/\.watch-badge,\s*\.held-badge \{[^}]*\}/g),
  grab(/\.watch-dot,\s*\.held-dot \{[^}]*\}/g),
  grab(/\n\.watch-dot \{[^}]*\}/g),
  grab(/\n\.held-dot \{[^}]*\}/g),
  grab(/@property --dot-alpha \{[^}]*\}/g),
  grab(/@keyframes watch-breathe \{[\s\S]*?\n\}/g),
].join('\n');

const BACKGROUNDS = { white: '#ffffff', black: '#000000', grey: '#808080', blue: '#2a4d8f' };
const STATES = {
  // Never animated.
  none: 'animation: none;',
  // The pulse has played and ended (what the founder saw).
  idle: 'animation-delay: -10s;',
  // Part way through the pulse.
  pulsing: 'animation-delay: -0.3s; animation-play-state: paused;',
};

function html(bg, state, kind) {
  const cls = kind === 'held' ? 'held-badge' : 'watch-badge';
  const dot = kind === 'held' ? 'held-dot' : 'watch-dot';
  return `<!doctype html><meta charset=utf-8><style>
    html,body{margin:0;background:${bg}} ${css}
    .area{position:absolute;left:100px;top:60px;width:200px;height:80px}
    .${dot}{${STATES[state]}}
  </style><div class="area"><span class="${cls}"><span class="${dot}"></span></span></div>`;
}

/**
 * How far the dot's pixels are from mirror symmetry, measured on what was painted: the
 * bounding box of the pixels that are not the background, searched in the box the page
 * gave the dot plus two pixels each way (the box is snapped to the screen's pixels, so
 * its edge can fall a pixel either side of where the layout puts it).
 */
function asymmetry(img, rect, scale, bg) {
  const x0 = Math.floor(rect.x * scale) - 2, y0 = Math.floor(rect.y * scale) - 2;
  const w = Math.ceil(rect.width * scale) + 5, h = Math.ceil(rect.height * scale) + 5;
  const px = (x, y) => { const o = ((y0 + y) * img.width + x0 + x) * img.bpp; return [img.px[o], img.px[o + 1], img.px[o + 2]]; };
  const away = (c) => Math.max(...c.map((v, i) => Math.abs(v - bg[i]))) > 2;
  let [minX, minY, maxX, maxY] = [w, h, -1, -1];
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) if (away(px(x, y))) { minX = Math.min(minX, x); maxX = Math.max(maxX, x); minY = Math.min(minY, y); maxY = Math.max(maxY, y); }
  const bw = maxX - minX + 1, bh = maxY - minY + 1;
  let worst = 0;
  for (let y = 0; y < bh; y++) for (let x = 0; x < bw; x++) {
    for (const [mx, my] of [[bw - 1 - x, y], [x, bh - 1 - y]]) {
      const a = px(minX + x, minY + y), b = px(minX + mx, minY + my);
      worst = Math.max(worst, Math.max(...a.map((v, i) => Math.abs(v - b[i]))));
    }
  }
  return { worst, w: bw, h: bh, at: [x0 + minX, y0 + minY] };
}

const { port, stop } = await startEdge();
const rows = []; const tiles = [];
let failures = 0;
for (const scale of [1, 1.25, 1.5, 2]) {
  for (const kind of ['watching', 'held']) {
    for (const [bgName, bg] of Object.entries(BACKGROUNDS)) {
      for (const state of kind === 'held' ? ['none'] : Object.keys(STATES)) {
        const cdp = await newTab(port);
        await cdp.send('Page.enable');
        await cdp.send('Emulation.setDeviceMetricsOverride', { width: 400, height: 200, deviceScaleFactor: scale, mobile: false });
        const file = join(out, `dot_${kind}_${bgName}_${state}_s${scale}.html`);
        writeFileSync(file, html(bg, state, kind));
        await cdp.send('Page.navigate', { url: pathToFileURL(file).href });
        await new Promise((r) => setTimeout(r, 400));
        const rect = JSON.parse((await cdp.send('Runtime.evaluate', { expression: `JSON.stringify(document.querySelector('.${kind === 'held' ? 'held-dot' : 'watch-dot'}').getBoundingClientRect())`, returnByValue: true })).result.value);
        const shot = await cdp.send('Page.captureScreenshot', { format: 'png' });
        const img = decodePng(Buffer.from(shot.data, 'base64'));
        const hex = bg.slice(1).match(/../g).map((v) => parseInt(v, 16));
        const r = asymmetry(img, rect, scale, hex);
        // Everything outside the circle's own box must still be the background.
        rows.push(`${kind.padEnd(8)} ${bgName.padEnd(6)} ${state.padEnd(8)} scale ${String(scale).padEnd(4)} painted ${r.w}x${r.h}  mirror error ${r.worst}`);
        // A dot that painted nothing, or a box far from the 6 CSS pixels it was given, is
        // not a circle at all: the symmetry test alone would pass an empty box (review of
        // #178, finding 1).
        const side = 6 * scale;
        const sized = r.w > 0 && r.h > 0 && Math.abs(r.w - r.h) <= 1 && Math.abs(r.w - side) <= 2;
        if (!sized) rows.push(`  ^ FAIL: painted ${r.w}x${r.h}, expected about ${side}x${side}`);
        if (!sized || r.worst > 24) failures++;
        // 12x12 physical pixels around the dot, for the montage.
        const x0 = r.at[0] - 2, y0 = r.at[1] - 2;
        const tile = Buffer.alloc(12 * 12 * 3);
        for (let y = 0; y < 12; y++) for (let x = 0; x < 12; x++) for (let c = 0; c < 3; c++) tile[(y * 12 + x) * 3 + c] = img.px[((y0 + y) * img.width + x0 + x) * img.bpp + c];
        tiles.push({ label: `${kind} ${bgName} ${state} ${scale}`, tile });
        cdp.close();
      }
    }
  }
}
stop();
console.log(rows.join('\n'));
writeFileSync(join(out, 'tiles.json'), JSON.stringify(tiles.map((t) => ({ label: t.label, rgb: t.tile.toString('base64') }))));
console.log(failures ? `${failures} states are not a symmetric circle` : 'every state is a symmetric circle');
process.exit(failures ? 1 : 0);
