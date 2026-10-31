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
