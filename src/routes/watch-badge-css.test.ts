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
  const block = page.match(/\.watch-badge \{([^}]*)\}/)?.[1] ?? '';

  it('is clipped to the area', () => {
    expect(block).toContain('overflow: hidden;');
    expect(block).toContain('max-width: calc(100% - 8px);');
    expect(block).toContain('max-height: calc(100% - 4px);');
  });

  it('starts from the top right corner', () => {
    expect(block).toContain('right: 4px;');
    expect(block).toContain('top: 2px;');
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
  const block = page.match(/\.watch-dot \{([^}]*)\}/)?.[1] ?? '';

  it('is laid out as its line box, not as its glyph', () => {
    expect(block).toContain('display: inline-block;');
    expect(block).toContain('line-height: 1;');
  });
});
