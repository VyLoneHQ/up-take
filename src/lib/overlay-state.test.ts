import { describe, expect, it, vi } from 'vitest';

import {
  type AreaView,
  areaFramesCss,
  dismissFocusedArea,
  escapeOverlay,
  isRemoveKey,
  type MenuView,
  menuFrameCss,
  monitorFramesCss,
  type PhysRect,
  physRectsToCss,
  physRectToCss,
  showsMenu,
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

describe('physRectsToCss', () => {
  it('offsets by the origin and divides by dpr, like the monitor frames', () => {
    // An area at physical (100, 200) on a desktop whose origin is (-1080, -1080),
    // viewed at 125%: it sits 1180/1.25 = 944 px right of the overlay top-left.
    expect(
      physRectsToCss([[100, 200, 800, 600]], [-1080, -1080], 1.25),
    ).toEqual([{ x: 944, y: 1024, width: 640, height: 480 }]);
  });

  it('returns nothing for a non-finite or non-positive dpr', () => {
    for (const dpr of [Number.NaN, 0, -2, Number.POSITIVE_INFINITY]) {
      expect(physRectsToCss([[0, 0, 10, 10]], [0, 0], dpr)).toEqual([]);
    }
  });

  it('maps an empty list to an empty list', () => {
    expect(physRectsToCss([], [0, 0], 1)).toEqual([]);
  });
});

describe('physRectToCss', () => {
  it('converts a single physical rect', () => {
    expect(physRectToCss([100, 200, 800, 600], [0, 0], 2)).toEqual({
      x: 50,
      y: 100,
      width: 400,
      height: 300,
    });
  });

  it('passes null through as null — nothing to draw', () => {
    expect(physRectToCss(null, [0, 0], 1)).toBeNull();
  });

  it('returns null for an unusable dpr rather than a NaN-positioned box', () => {
    expect(physRectToCss([0, 0, 10, 10], [0, 0], 0)).toBeNull();
    expect(physRectToCss([0, 0, 10, 10], [0, 0], Number.NaN)).toBeNull();
  });
});

describe('showsTint', () => {
  it('tints and frames only in placement', () => {
    expect(showsTint('placement')).toBe(true);
    expect(showsTint('living')).toBe(false);
    expect(showsTint('hidden')).toBe(false);
  });
});

describe('showsMenu', () => {
  it('allows the area menu in every visible state', () => {
    // Living included (ADR-0016): the menu opens there on a right-click over an
    // interactive area, and a Placement-only gate would leave Rust hit-testing
    // a menu the user cannot see.
    expect(showsMenu('placement')).toBe(true);
    expect(showsMenu('living')).toBe(true);
    expect(showsMenu('hidden')).toBe(false);
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

describe('areaFramesCss', () => {
  const areas: AreaView[] = [
    {
      id: 7,
      rect: [100, 100, 200, 150],
      close: [282, 100, 18, 18],
      layer: 'auto',
    },
    {
      id: 9,
      rect: [-1000, -200, 300, 300],
      close: [-718, -200, 18, 18],
      layer: 'front',
    },
  ];

  it('converts the area and its close control against the same origin and scale', () => {
    const [first] = areaFramesCss(areas, [-1080, -1080], 2, null);

    expect(first?.id).toBe(7);
    expect(first?.rect).toEqual({ x: 590, y: 590, width: 100, height: 75 });
    // The control must land on the area's own top-right, not near it: it is the
    // rectangle Rust hit-tests, so a drift here is a control that is drawn in
    // one place and clickable in another.
    expect(first?.close).toEqual({ x: 681, y: 590, width: 9, height: 9 });
  });

  it('carries the layer tier and marks only the hovered area', () => {
    const frames = areaFramesCss(areas, [0, 0], 1, 9);

    expect(frames.map((frame) => frame.hovered)).toEqual([false, true]);
    expect(frames.map((frame) => frame.layer)).toEqual(['auto', 'front']);
  });

  it('marks the dragged area as the source and not as hovered', () => {
    // A move must never look like two areas. The source is styled as where the
    // area is coming from; the hover chrome is suppressed because its close
    // control would sit at the source while the cursor is elsewhere.
    const frames = areaFramesCss(areas, [0, 0], 1, 9, 9);

    expect(frames.map((frame) => frame.source)).toEqual([false, true]);
    expect(frames.map((frame) => frame.hovered)).toEqual([false, false]);
  });

  it('leaves every area normal when no drag is in progress', () => {
    // The restore path: cancelling a drag clears the source, and the styling
    // follows because it is derived rather than stored.
    const frames = areaFramesCss(areas, [0, 0], 1, null, null);

    expect(frames.every((frame) => !frame.source)).toBe(true);
  });

  it('draws nothing at all when the scale is unusable', () => {
    // Matching physRectsToCss: a NaN-positioned area still covers the screen
    // while being unclickable, which is worse than no area drawn.
    expect(areaFramesCss(areas, [0, 0], Number.NaN, null)).toEqual([]);
    expect(areaFramesCss(areas, [0, 0], 0, null)).toEqual([]);
  });
});

describe('menuFrameCss', () => {
  const menu: MenuView = {
    rect: [400, 300, 176, 122],
    items: [
      { rect: [400, 305, 176, 28], label: 'Always on top', checked: false },
      { rect: [400, 333, 176, 28], label: 'Auto', checked: true },
    ],
    hovered: 1,
  };

  it('positions every row from the rect Rust hit-tests', () => {
    const frame = menuFrameCss(menu, [0, 0], 1);

    expect(frame?.rect).toEqual({ x: 400, y: 300, width: 176, height: 122 });
    expect(frame?.items[0]?.rect).toEqual({
      x: 400,
      y: 305,
      width: 176,
      height: 28,
    });
    expect(frame?.items.map((item) => item.hovered)).toEqual([false, true]);
    expect(frame?.items.map((item) => item.checked)).toEqual([false, true]);
  });

  it('is null when no menu is open or the scale is unusable', () => {
    expect(menuFrameCss(null, [0, 0], 1)).toBeNull();
    expect(menuFrameCss(menu, [0, 0], Number.NaN)).toBeNull();
  });
});

describe('isRemoveKey', () => {
  it('removes on Delete only', () => {
    expect(isRemoveKey('Delete')).toBe(true);
    expect(isRemoveKey('Escape')).toBe(false);
  });

  it('does not treat Backspace as a remove key', () => {
    // Deliberate: Backspace is the reflexive "undo that" key, and dismissing an
    // area has no undo.
    expect(isRemoveKey('Backspace')).toBe(false);
  });
});

describe('dismissFocusedArea', () => {
  it('asks Rust to dismiss the area under the cursor', async () => {
    const invoke = vi.fn<Invoke>().mockResolvedValue(undefined);

    await expect(dismissFocusedArea(invoke)).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith('overlay_dismiss_focused');
  });

  it('resolves false instead of throwing when the command fails', async () => {
    const invoke = vi.fn<Invoke>().mockRejectedValue(new Error('no window'));

    await expect(dismissFocusedArea(invoke)).resolves.toBe(false);
  });
});
