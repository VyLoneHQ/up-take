import { describe, expect, it, vi } from 'vitest';

import {
  type AreaView,
  areaFramesCss,
  armAreaType,
  armedTypeForKey,
  dismissFocusedArea,
  escapeOverlay,
  isFreezeKey,
  isRemoveKey,
  type MenuView,
  menuFrameCss,
  monitorFramesCss,
  type PhysRect,
  physRectsToCss,
  physRectToCss,
  reportFreezeLatency,
  showsMenu,
  showsTint,
  stillsFromWire,
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

describe('stillsFromWire', () => {
  it('keeps each still with its own rect and url', () => {
    const stills = stillsFromWire([
      [-1920, -200, 1920, 1080, 'http://s.localhost/frozen-0-7.png'],
      [0, 0, 2560, 1440, 'http://s.localhost/frozen-1-7.png'],
    ]);
    expect(stills).toEqual([
      {
        rect: [-1920, -200, 1920, 1080],
        url: 'http://s.localhost/frozen-0-7.png',
      },
      { rect: [0, 0, 2560, 1440], url: 'http://s.localhost/frozen-1-7.png' },
    ]);
  });

  it('does not pair a rect with another still url', () => {
    // The tuple order is the whole contract, and getting it wrong would show
    // monitor 0's pixels over monitor 1 — plausible on screen and wrong.
    const [first, second] = stillsFromWire([
      [10, 20, 30, 40, 'a'],
      [50, 60, 70, 80, 'b'],
    ]);
    expect(first.url).toBe('a');
    expect(first.rect).toEqual([10, 20, 30, 40]);
    expect(second.url).toBe('b');
    expect(second.rect).toEqual([50, 60, 70, 80]);
  });

  it('is empty for a live screen', () => {
    expect(stillsFromWire([])).toEqual([]);
  });
});

describe('isFreezeKey', () => {
  const key = (over: Partial<KeyboardEvent> & { key: string }) => ({
    ctrlKey: false,
    altKey: false,
    metaKey: false,
    ...over,
  });

  it('fires on Ctrl+Space', () => {
    expect(isFreezeKey(key({ key: ' ', ctrlKey: true }))).toBe(true);
  });

  it('does not fire on Space alone', () => {
    // Space is deliberately unclaimed (ADR-0026 decision 8). If this ever
    // starts passing, a future area-level binding has been taken by accident.
    expect(isFreezeKey(key({ key: ' ' }))).toBe(false);
  });

  it('does not fire when Alt or Meta are also held', () => {
    // Alt already suppresses snapping during placement, and Win+Space is the
    // Windows layout switcher — neither chord is ours to claim.
    expect(isFreezeKey(key({ key: ' ', ctrlKey: true, altKey: true }))).toBe(
      false,
    );
    expect(isFreezeKey(key({ key: ' ', ctrlKey: true, metaKey: true }))).toBe(
      false,
    );
  });

  it('tests event.key and not event.code', () => {
    // The space bar's `key` is a single space; `'Space'` is its `code`. Testing
    // the wrong one gives a binding that never fires and reads as a dead
    // feature — I-11's shape, where silence and working look identical.
    expect(isFreezeKey(key({ key: 'Space', ctrlKey: true }))).toBe(false);
  });

  it('does not swallow Ctrl+S or other Ctrl chords', () => {
    expect(isFreezeKey(key({ key: 's', ctrlKey: true }))).toBe(false);
  });
});

describe('armedTypeForKey', () => {
  const key = (over: Partial<KeyboardEvent> & { key: string }) => ({
    ctrlKey: false,
    altKey: false,
    metaKey: false,
    ...over,
  });

  it('arms Screenshot on S, in either case', () => {
    expect(armedTypeForKey(key({ key: 's' }))).toBe('screenshot');
    expect(armedTypeForKey(key({ key: 'S' }))).toBe('screenshot');
  });

  it('still arms when Shift is held, since that is how a capital S is typed', () => {
    expect(armedTypeForKey(key({ key: 'S', shiftKey: true }))).toBe(
      'screenshot',
    );
  });

  it('arms nothing under Ctrl, Alt or Meta', () => {
    // Alt is the one that matters: it already suppresses snapping during
    // placement, so Alt+S is a chord the user may well press for that reason.
    expect(armedTypeForKey(key({ key: 's', ctrlKey: true }))).toBeNull();
    expect(armedTypeForKey(key({ key: 's', altKey: true }))).toBeNull();
    expect(armedTypeForKey(key({ key: 's', metaKey: true }))).toBeNull();
  });

  it('arms nothing for keys with no type', () => {
    expect(armedTypeForKey(key({ key: 'q' }))).toBeNull();
    expect(armedTypeForKey(key({ key: 'Escape' }))).toBeNull();
    expect(armedTypeForKey(key({ key: 'Delete' }))).toBeNull();
  });
});

describe('armAreaType', () => {
  it('asks Rust to arm the type of the next drag', async () => {
    const invoke = vi.fn<Invoke>().mockResolvedValue(undefined);

    await expect(armAreaType(invoke, 'screenshot')).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith('overlay_arm_type', {
      kind: 'screenshot',
    });
  });

  it('resolves false without logging when Rust refuses', async () => {
    // Rust rejects arming outside placement, and the key handler is live in
    // living too — so this rejection is the expected path, not an error.
    const invoke = vi
      .fn<Invoke>()
      .mockRejectedValue(new Error('not in placement'));

    await expect(armAreaType(invoke, 'screenshot')).resolves.toBe(false);
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

describe('reportFreezeLatency', () => {
  /**
   * The property that makes this function different from `reportLatency`, and
   * the only reason it exists: the clock must stop after the stills have
   * **decoded**, not when the DOM updated. `quality-bars.md` §1's *frozen view
   * painted* row is about pixels on screen, and 1.9f's measured 72–78 ms stops
   * at the encode — so a probe that echoed before decode would report the one
   * unmeasured stage as free.
   */
  it('does not echo until every still has decoded', async () => {
    const frames: (() => void)[] = [];
    vi.stubGlobal('requestAnimationFrame', (fn: () => void) => {
      frames.push(fn);
      return frames.length;
    });
    const invoke = vi.fn().mockResolvedValue(undefined);
    let releaseFirst!: () => void;
    const images = [
      {
        decode: () =>
          new Promise<void>((resolve) => {
            releaseFirst = resolve;
          }),
      },
      { decode: () => Promise.resolve() },
    ] as unknown as HTMLImageElement[];

    const done = reportFreezeLatency(invoke, 4242, images);
    // One decode is still outstanding, so nothing may have been scheduled yet.
    await Promise.resolve();
    expect(frames).toHaveLength(0);
    expect(invoke).not.toHaveBeenCalled();

    releaseFirst();
    await done;
    // Two nested frames, and only the inner one echoes.
    expect(frames).toHaveLength(1);
    frames[0]?.();
    expect(frames).toHaveLength(2);
    expect(invoke).not.toHaveBeenCalled();
    frames[1]?.();
    expect(invoke).toHaveBeenCalledWith('overlay_report_freeze_latency', {
      probe: 4242,
    });
    vi.unstubAllGlobals();
  });

  /**
   * A still whose image 404s must not strand the measurement. Silence is the one
   * outcome a probe cannot have (`I-11`): it is indistinguishable from the
   * probe being switched off.
   */
  it('still echoes when a decode rejects', async () => {
    const frames: (() => void)[] = [];
    vi.stubGlobal('requestAnimationFrame', (fn: () => void) => {
      frames.push(fn);
      return frames.length;
    });
    const invoke = vi.fn().mockResolvedValue(undefined);
    const images = [
      { decode: () => Promise.reject(new Error('404')) },
    ] as unknown as HTMLImageElement[];

    await reportFreezeLatency(invoke, 7, images);
    frames[0]?.();
    frames[1]?.();
    expect(invoke).toHaveBeenCalledWith('overlay_report_freeze_latency', {
      probe: 7,
    });
    vi.unstubAllGlobals();
  });
});
