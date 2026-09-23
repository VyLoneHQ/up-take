import { describe, expect, it } from 'vitest';
import { hiddenClipPath } from './freeze-mask';

describe('hiddenClipPath', () => {
  it('clips nothing when nothing is hidden', () => {
    expect(hiddenClipPath([])).toBeNull();
  });

  it('cuts one hole per covered monitor, and only those', () => {
    const clip = hiddenClipPath([{ x: 0, y: 0, width: 1280, height: 720 }]);
    expect(clip).toBe(
      "path(evenodd, 'M-1000000 -1000000H1000000V1000000H-1000000ZM0 0h1280v720h-1280Z')",
    );
  });

  it('keeps adjacent monitors as separate holes that only share an edge', () => {
    // Two monitors side by side: the second starts exactly where the first
    // ends, so the holes touch and do not overlap, and even-odd leaves both
    // hidden rather than re-filling a shared strip.
    const clip = hiddenClipPath([
      { x: 0, y: 0, width: 1280, height: 720 },
      { x: 1280, y: 0, width: 960, height: 540 },
    ]);
    expect(clip).toContain('M0 0h1280v720h-1280Z');
    expect(clip).toContain('M1280 0h960v540h-960Z');
    expect(clip?.match(/Z/g)).toHaveLength(3);
  });

  it('handles monitors left of and above the primary', () => {
    const clip = hiddenClipPath([
      { x: -1080, y: -274, width: 1080, height: 1920 },
    ]);
    expect(clip).toContain('M-1080 -274h1080v1920h-1080Z');
  });
});
