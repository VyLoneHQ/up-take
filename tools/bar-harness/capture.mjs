// Renders the bar test page in headless Edge and writes grey fixtures for the
// ignored detector test (`crates/uptake-core/tests/bar_fixtures.rs`).
//
// node tools/bar-harness/capture.mjs --out target/bar-fixtures [--step 0.5]
//      [--styles round,thin] [--themes light,dark] [--scales 1,1.5]
//      [--margins 8,0,label,8+1,8+2] [--jobs 4] [--edge <msedge.exe>]
//
// One file per style x theme x scale x margin, `<style>_<theme>_s<scale>_m<margin>.bar`:
//   "BARF" u32 width u32 height u32 frames, then per frame f32 value (percent, or
//   the sweep phase in percent for the marquee) followed by width*height luma bytes
//   (the formula of `uptake_core::watch::luma`, which is all a fingerprint reads).
// Node 22+ (global WebSocket and fetch), no packages.
import { mkdirSync, writeFileSync } from 'node:fs';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { DEFAULT_EDGE, decodePng, newTab, startEdge } from './lib.mjs';

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => {
    if (a.startsWith('--')) acc.push([a.slice(2), all[i + 1] && !all[i + 1].startsWith('--') ? all[i + 1] : 'true']);
    return acc;
  }, []),
);
const here = dirname(fileURLToPath(import.meta.url));
const out = resolve(args.out ?? 'target/bar-fixtures');
const step = parseFloat(args.step ?? '0.5');
const ALL = ['round', 'square', 'thin', 'thick', 'gradient', 'striped', 'shimmer', 'segmented', 'rtl', 'vertical', 'marquee'];
const styles = (args.styles ?? ALL.join(',')).split(',');
const themes = (args.themes ?? 'light,dark').split(',');
const scales = (args.scales ?? '1,1.5').split(',').map(Number);
const margins = (args.margins ?? '8,24,label').split(',');
const jobs = parseInt(args.jobs ?? '4', 10);
const edge = args.edge ?? DEFAULT_EDGE;
const page = pathToFileURL(join(here, 'bar-test.html')).href;
mkdirSync(out, { recursive: true });

async function one(port, style, theme, scale, margin) {
  const cdp = await newTab(port);
  await cdp.send('Page.enable');
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 760, height: 420, deviceScaleFactor: scale, mobile: false });
  await cdp.send('Page.navigate', { url: `${page}?static=1&style=${style}&theme=${theme}` });
  for (let i = 0; i < 50; i++) { const r = await cdp.send('Runtime.evaluate', { expression: 'typeof window.setStatic', returnByValue: true }); if (r.result.value === 'function') break; await new Promise(r => setTimeout(r, 100)); }
  const rect = (await cdp.send('Runtime.evaluate', { expression: 'JSON.stringify(document.querySelector(".track").getBoundingClientRect())', returnByValue: true })).result.value;
  const t = JSON.parse(rect);
  // Margin tokens: `8` is 8 CSS px of page all round, `0` a tight area, `label` 24 px and
  // the percentage beside the bar, `8+1` 8 px with the area's left and top edge moved one
  // physical pixel in (an area is rarely drawn on the cell grid).
  const [base, jitter] = margin.split('+');
  const m = base === 'label' ? 24 : parseInt(base, 10);
  const right = base === 'label' ? 100 : m;
  const j = parseInt(jitter ?? '0', 10);
  const x0 = Math.max(0, Math.floor((t.x - m) * scale) + j), y0 = Math.max(0, Math.floor((t.y - m) * scale) + j);
  const x1 = Math.ceil((t.x + t.width + right) * scale), y1 = Math.ceil((t.y + t.height + m) * scale);
  const w = x1 - x0, h = y1 - y0;
  const marquee = style === 'marquee';
  const frames = []; const n = Math.round(100 / step) + 1;
  for (let i = 0; i < n; i++) {
    const v = Math.min(100, i * step);
    const phase = marquee ? (v / 100) * 1.5 : (i * 0.137) % 7;
    await cdp.send('Runtime.evaluate', { expression: `window.setStatic(${marquee ? 0 : v}, ${phase})` });
    const shot = await cdp.send('Page.captureScreenshot', { format: 'png' });
    const img = decodePng(Buffer.from(shot.data, 'base64'));
    const grey = Buffer.alloc(w * h);
    for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
      const o = ((y0 + y) * img.width + x0 + x) * img.bpp;
      grey[y * w + x] = Math.floor((299 * img.px[o] + 587 * img.px[o + 1] + 114 * img.px[o + 2]) / 1000);
    }
    frames.push([v, grey]);
  }
  cdp.close();
  const head = Buffer.alloc(16); head.write('BARF', 0, 'latin1'); head.writeUInt32LE(w, 4); head.writeUInt32LE(h, 8); head.writeUInt32LE(frames.length, 12);
  const parts = [head];
  for (const [v, grey] of frames) { const f = Buffer.alloc(4); f.writeFloatLE(v, 0); parts.push(f, grey); }
  const name = `${style}_${theme}_s${scale}_m${margin}.bar`;
  writeFileSync(join(out, name), Buffer.concat(parts));
  console.log(`${name}  ${w}x${h}  ${frames.length} frames`);
}

const { port, stop } = await startEdge(edge);
const queue = [];
for (const style of styles) for (const theme of themes) for (const scale of scales) for (const margin of margins) queue.push([style, theme, scale, margin]);
await Promise.all(Array.from({ length: jobs }, async () => { for (let job; (job = queue.shift());) await one(port, ...job); }));
await stop();
process.exit(0);
