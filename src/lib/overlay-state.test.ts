import { describe, expect, it, vi } from 'vitest';

import {
  escapeOverlay,
  monitorFramesCss,
  type PhysRect,
  showsTint,
} from './overlay-state';
import type { Invoke } from './regions';

describe('monitorFramesCss', () => {
  it('offsets each monitor by the overlay origin so frames sit in the viewport', () => {
    // The dev rig: a virtual desktop whose origin is (-1080, -1080). A monitor
    // at physical (0, 0) is 1080 px right and down of the overlay's top-left.
    const monitors: PhysRect[] = [
      [0, 0, 2560, 1440],
      [-1080, -1080, 1080, 1920],
    ];
    expect(monitorFramesCss(monitors, [-1080, -1080], 1)).toEqual([
      { x: 1080, y: 1080, width: 2560, height: 1440 },
      { x: 0, y: 0, width: 1080, height: 1920 },
    ]);
  });

  it('divides by devicePixelRatio so frames land in CSS space', () => {
    const [frame] = monitorFramesCss([[100, 200, 800, 600]], [0, 0], 1.25);
    expect(frame).toEqual({ x: 80, y: 160, width: 640, height: 480 });
  });

  it('returns no frames for a non-finite or non-positive dpr', () => {
    // A NaN dpr would place every frame at NaN; a garbage indicator is worse
    // than none. Same fail-safe as the Rust scale check (ADR-0011 fallout).
    for (const dpr of [Number.NaN, 0, -1, Number.POSITIVE_INFINITY]) {
      expect(monitorFramesCss([[0, 0, 100, 100]], [0, 0], dpr)).toEqual([]);
    }
  });

  it('reports nothing when there are no monitors', () => {
    expect(monitorFramesCss([], [0, 0], 1)).toEqual([]);
  });
});

describe('showsTint', () => {
  it('tints and frames only in placement', () => {
    expect(showsTint('placement')).toBe(true);
    expect(showsTint('living')).toBe(false);
    expect(showsTint('hidden')).toBe(false);
  });
});

describe('escapeOverlay', () => {
  it('emits the escape intent', async () => {
    const invoke = vi.fn<Invoke>().mockResolvedValue(undefined);

    await expect(escapeOverlay(invoke)).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith('overlay_escape');
  });

  it('resolves false instead of throwing when the intent fails', async () => {
    // Esc is a dismiss path; an unhandled rejection would strand the user with
    // the overlay holding focus.
    const invoke = vi.fn<Invoke>().mockRejectedValue(new Error('no window'));

    await expect(escapeOverlay(invoke)).resolves.toBe(false);
  });
});
