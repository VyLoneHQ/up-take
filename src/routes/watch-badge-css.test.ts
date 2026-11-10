import { describe, expect, it } from 'vitest';
import page from './+page.svelte?raw';

/**
 * The host reads a watch area only while the area's own top right corner is
 * on a monitor (`src-tauri/src/watch.rs`, `mark`). That says the *watching*
 * mark is on screen only if the page keeps the mark inside the area, which
 * the badge's CSS does by clipping it. Rounds 15 and 16 of the review of #154
 * found the mark running past a narrow area's edge, and then the host
 * refusing small areas whose mark showed. jsdom does no layout, so the rule
 * is pinned in the source.
 */
describe('the watching mark stays inside its area', () => {
  const block =
    page.match(/\.watch-badge,\s*\.held-badge \{([^}]*)\}/)?.[1] ?? '';

  it('is clipped to the area', () => {
    expect(block).toContain('overflow: hidden;');
    expect(block).toContain('max-width: calc(100% - 2px);');
    expect(block).toContain('max-height: calc(100% - 2px);');
  });

  it('starts from the top left corner (the maintainer, 2026-11-10)', () => {
    expect(block).toContain('left: 1px;');
    expect(block).toContain('top: 1px;');
  });
});

/**
 * The page confirms a mark only when its dot lies wholly inside the badge
 * ($lib/mark-check, `markSeen`). A plain inline span's box is the whole glyph
 * of the font that draws the dot, taller than the badge's 11px line, so no
 * mark was ever confirmed and no watch area read on a real screen (found at
 * the maintainer's rig, 2026-11-03; measured in headless Edge). jsdom does no
 * layout, so the rule that makes the dot's box its line box is pinned here.
 */
describe('the watching dot fits inside its badge', () => {
  const block = page.match(/\.watch-dot,\s*\.held-dot \{([^}]*)\}/)?.[1] ?? '';

  it('is a drawn box of a fixed size, not a glyph', () => {
    expect(block).toContain('display: block;');
    expect(block).toContain('width: 6px;');
    expect(block).toContain('height: 6px;');
  });

  // The maintainer's rig pass of 2026-11-10: an area as small as two
  // letters never read, because the word "watching" did not fit and a mark
  // not seen is a mark not read. With a 6 px dot 1 px in from the corner, an
  // area 8 CSS pixels on a side shows it whole.
  it('fits an area 8 pixels on a side', () => {
    const badge =
      page.match(/\.watch-badge,\s*\.held-badge \{([^}]*)\}/)?.[1] ?? '';
    const inset = Number(badge.match(/top: (\d+)px;/)?.[1]);
    const side = Number(block.match(/width: (\d+)px;/)?.[1]);
    expect(inset + side + inset).toBeLessThanOrEqual(8);
  });
});

/**
 * The maintainer, 2026-11-10: the dot animates while it watches, subtly,
 * and the two states stay easy to tell apart. Pinned in the source, since
 * jsdom runs no animation: the watching dot breathes by opacity only (its
 * box, which markSeen measures, never moves), the paused ring has no
 * animation, and reduced motion stops the breathing.
 */
describe('the status dot', () => {
  const watching = page.match(/\n\.watch-dot \{([^}]*)\}/)?.[1] ?? '';
  // Its own rule, the last `.held-dot {`: the first is shared with the
  // watching dot's size.
  const paused =
    [...page.matchAll(/\n\.held-dot \{([^}]*)\}/g)].at(-1)?.[1] ?? '';
  const breathe =
    page.match(/@keyframes watch-breathe \{([\s\S]*?)\n\}/)?.[1] ?? '';

  it('breathes while it watches, by opacity alone', () => {
    expect(watching).toContain('animation: watch-breathe');
    expect(breathe).toContain('opacity');
    expect(breathe).not.toMatch(/width|height|transform|top|left|margin/);
  });

  it('holds still while paused, and looks different without colour', () => {
    expect(paused).not.toContain('animation');
    expect(paused).toContain('inset 0 0 0 1.5px');
  });

  it('stops breathing when Windows asks for less motion', () => {
    expect(page).toMatch(
      /@media \(prefers-reduced-motion: reduce\) \{\s*\.watch-dot \{\s*animation: none;/,
    );
  });
});
