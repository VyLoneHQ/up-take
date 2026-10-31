import { describe, expect, it } from 'vitest';
import { type Box, markSeen } from './mark-check';

const box = (x: number, y: number, width: number, height: number): Box => ({
  x,
  y,
  width,
  height,
});

// One 1920 by 1080 monitor and a shorter one to its right.
const monitors = [box(0, 0, 1920, 1080), box(1920, 0, 1280, 720)];
// A badge in an ordinary area, its dot at the badge's left end.
const badge = box(500, 102, 60, 11);
const dot = box(500, 102, 8, 11);

describe('a watching mark is seen', () => {
  it('when its dot is inside the badge, on a monitor, and uncovered', () => {
    expect(markSeen(dot, badge, monitors, [])).toBe(true);
  });

  it('not when the badge is clipped smaller than the dot (round 17)', () => {
    expect(markSeen(dot, box(500, 102, 4, 11), monitors, [])).toBe(false);
    expect(markSeen(dot, box(500, 102, 60, 6), monitors, [])).toBe(false);
  });

  it('not when the dot is off the desktop or in a gap (rounds 3, 15, 19)', () => {
    const off = box(-6, 102, 8, 11);
    expect(markSeen(off, box(-6, 102, 60, 11), monitors, [])).toBe(false);
    const gap = box(2000, 715, 8, 11);
    expect(markSeen(gap, box(2000, 715, 60, 11), monitors, [])).toBe(false);
  });

  it('not when an area drawn above it overlaps the dot (round 14)', () => {
    expect(markSeen(dot, badge, monitors, [box(495, 100, 20, 20)])).toBe(false);
    expect(markSeen(dot, badge, monitors, [box(520, 100, 20, 20)])).toBe(true);
  });

  it('not when nothing was laid out', () => {
    expect(markSeen(box(0, 0, 0, 0), badge, monitors, [])).toBe(false);
  });
});
