// Shared by capture.mjs and dot-check.mjs: start headless Edge, talk CDP, decode a screenshot.
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { inflateSync } from 'node:zlib';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

export const DEFAULT_EDGE = 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe';

/** Starts headless Edge on a random port; returns {port, stop}. */
export async function startEdge(edge = DEFAULT_EDGE) {
  const port = 9300 + Math.floor(Math.random() * 500);
  const profile = mkdtempSync(join(tmpdir(), 'bar-edge-'));
  const browser = spawn(
    edge,
    ['--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, '--hide-scrollbars',
      '--force-color-profile=srgb', '--no-first-run', '--no-default-browser-check', 'about:blank'],
    { stdio: 'ignore' },
  );
  // Stopping waits for Edge to exit and then removes its profile, so a caller that awaits
  // it and then exits leaves nothing behind (review of #178, round 2, finding 1). A process
  // that dies some other way still kills Edge on exit, synchronously; the profile is then
  // left in the temp folder, named bar-edge-*.
  const exited = new Promise((done) => browser.once('exit', done));
  const kill = () => { try { browser.kill(); } catch {} };
  process.on('exit', kill);
  const stop = async () => {
    kill();
    await Promise.race([exited, new Promise((r) => setTimeout(r, 5000))]);
    for (let i = 0; i < 10; i++) {
      try { rmSync(profile, { recursive: true, force: true }); return; } catch { await new Promise((r) => setTimeout(r, 300)); }
    }
    throw new Error(`could not remove the Edge profile ${profile}`);
  };
  for (let i = 0; i < 100; i++) {
    try { await (await fetch(`http://127.0.0.1:${port}/json/version`)).json(); return { port, stop }; } catch { await new Promise(r => setTimeout(r, 200)); }
  }
  await stop().catch(() => {});
  throw new Error('Edge did not start');
}

/** A new tab: {cdp, close}. */
export async function newTab(port) {
  const target = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' })).json();
  const cdp = new Cdp(target.webSocketDebuggerUrl); await cdp.ready;
  return cdp;
}

export class Cdp {
  constructor(url) { this.ws = new WebSocket(url); this.id = 0; this.waiting = new Map();
    this.ready = new Promise((ok, no) => { this.ws.onopen = ok; this.ws.onerror = no; });
    this.ws.onmessage = e => { const m = JSON.parse(e.data); const w = this.waiting.get(m.id); if (w) { this.waiting.delete(m.id); m.error ? w.no(new Error(JSON.stringify(m.error))) : w.ok(m.result); } }; }
  send(method, params = {}) { const id = ++this.id; this.ws.send(JSON.stringify({ id, method, params })); return new Promise((ok, no) => this.waiting.set(id, { ok, no })); }
  close() { this.ws.close(); }
}

/** PNG (8-bit, RGB or RGBA, not interlaced) to {width, height, rgba}. */
export function decodePng(buf) {
  let at = 8, width = 0, height = 0, type = 0; const idat = [];
  while (at < buf.length) {
    const len = buf.readUInt32BE(at), kind = buf.toString('latin1', at + 4, at + 8), body = buf.subarray(at + 8, at + 8 + len);
    if (kind === 'IHDR') { width = body.readUInt32BE(0); height = body.readUInt32BE(4); type = body[9]; if (body[8] !== 8 || body[12] !== 0) throw new Error('png: 8-bit, not interlaced only'); }
    if (kind === 'IDAT') idat.push(body);
    at += 12 + len;
  }
  const bpp = type === 6 ? 4 : type === 2 ? 3 : 0; if (!bpp) throw new Error('png colour type ' + type);
  const raw = inflateSync(Buffer.concat(idat)), stride = width * bpp, px = Buffer.alloc(height * stride);
  for (let y = 0; y < height; y++) {
    const f = raw[y * (stride + 1)], src = y * (stride + 1) + 1, dst = y * stride;
    for (let i = 0; i < stride; i++) {
      const x = raw[src + i], a = i >= bpp ? px[dst + i - bpp] : 0, b = y ? px[dst - stride + i] : 0, c = i >= bpp && y ? px[dst - stride + i - bpp] : 0;
      let v;
      if (f === 0) v = x; else if (f === 1) v = x + a; else if (f === 2) v = x + b; else if (f === 3) v = x + ((a + b) >> 1);
      else { const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c); v = x + (pa <= pb && pa <= pc ? a : pb <= pc ? b : c); }
      px[dst + i] = v & 255;
    }
  }
  return { width, height, bpp, px };
}
