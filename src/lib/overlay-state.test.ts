import { describe, expect, it, vi } from 'vitest';

import {
  type AreaView,
  areaFramesCss,
  armAreaType,
  armedTypeForKey,
  dismissFocusedArea,
  escapeOverlay,
  formatZoom,
  frameKey,
  frozenFrameKeys,
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
      kind: 'default',
      zoom: 1,
      // 200x150, so it is above `CHROME_INSIDE_SPAN` on both axes: bands
      // inside, and therefore no outside handles. Rust decides this; the
      // fixture records what Rust sends for an area of this size.
      bar: [100, 82, 200, 18],
      handles: [],
    },
    {
      id: 9,
      rect: [-1000, -200, 300, 300],
      close: [-718, -200, 18, 18],
      layer: 'front',
      kind: 'filter',
      zoom: 1,
      bar: [-1000, -218, 300, 18],
      handles: [],
    },
  ];

  it('carries each area type through to the frame', () => {
    // The frame is what the template styles on, so a dropped `kind` renders
    // every Filter area as a plain one and nothing else goes wrong: no error,
    // no failing geometry test, just the tint silently absent.
    const frames = areaFramesCss(areas, [-1080, -1080], 2, null);

    expect(frames.map((frame) => frame.kind)).toEqual(['default', 'filter']);
  });

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

  it('gives a chrome-only hover its close control but not the highlight', () => {
    // Finding 1 of the independent review of #56. One id used to drive both the
    // control and the `.area.hovered` styling, whose own CSS comment defines it
    // as "which area a drag will grab". A pass-through body grants no such grab,
    // so a large Filter area sat permanently lit with a permanent close control
    // over the user's content. The two facts travel separately now.
    const frames = areaFramesCss(areas, [0, 0], 1, 9, null, true);

    expect(frames.map((frame) => frame.hovered)).toEqual([false, false]);
    expect(frames.map((frame) => frame.showClose)).toEqual([false, true]);
  });

  it('gives a grabbable hover both, which is the unchanged case', () => {
    // The other side of the same split: where a press does have a target, the
    // highlight is a true claim and nothing about today's behaviour moves.
    const frames = areaFramesCss(areas, [0, 0], 1, 9, null, false);

    expect(frames.map((frame) => frame.hovered)).toEqual([false, true]);
    expect(frames.map((frame) => frame.showClose)).toEqual([false, true]);
  });

  it('suppresses a dragged area entirely, chrome-only or not', () => {
    // The dragged area's control would sit at the source position while the
    // cursor is elsewhere, so neither flag may survive the drag.
    const frames = areaFramesCss(areas, [0, 0], 1, 9, 9, true);

    expect(frames.map((frame) => frame.hovered)).toEqual([false, false]);
    expect(frames.map((frame) => frame.showClose)).toEqual([false, false]);
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
      {
        rect: [400, 305, 176, 28],
        label: 'Area type',
        checked: false,
        parent: true,
      },
      {
        rect: [400, 333, 176, 28],
        label: 'Auto',
        checked: true,
        parent: false,
      },
    ],
    hovered: 1,
    child: null,
  };

  /** The same menu with its type list open, as Rust lays it out beside it. */
  const withChild: MenuView = {
    ...menu,
    child: {
      rect: [576, 300, 176, 94],
      items: [
        {
          rect: [576, 305, 176, 28],
          label: 'Type: Default',
          checked: true,
          parent: false,
        },
        {
          rect: [576, 333, 176, 28],
          label: 'Type: Screenshot',
          checked: false,
          parent: false,
        },
      ],
      hovered: 0,
      owner: 0,
    },
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

  it('marks the row that opens a child list and no other', () => {
    const frame = menuFrameCss(menu, [0, 0], 1);

    expect(frame?.items.map((item) => item.parent)).toEqual([true, false]);
  });

  it('draws no child list until one is open', () => {
    expect(menuFrameCss(menu, [0, 0], 1)?.child).toBeNull();
  });

  it('positions the child list from its own rects, not the parent list', () => {
    // The child opens flush beside the panel, so its x is the parent's right
    // edge. Deriving it here instead of reading it would put the rows where
    // this side thinks they are rather than where Rust hit-tests them, which
    // is the failure the whole menu is laid out in Rust to avoid.
    const frame = menuFrameCss(withChild, [0, 0], 1);

    expect(frame?.child?.rect).toEqual({
      x: 576,
      y: 300,
      width: 176,
      height: 94,
    });
    expect(frame?.child?.items[1]?.rect).toEqual({
      x: 576,
      y: 333,
      width: 176,
      height: 28,
    });
    expect(frame?.child?.items.map((item) => item.checked)).toEqual([
      true,
      false,
    ]);
  });

  it('highlights the child list from its own hovered index', () => {
    // Two lists, two indices. Reading the parent's `hovered` for both would
    // light row 1 of the child list here, which is a plausible highlight on the
    // wrong row: it looks like a working menu and points at the wrong type.
    const frame = menuFrameCss(withChild, [0, 0], 1);

    expect(frame?.items.map((item) => item.hovered)).toEqual([false, true]);
    expect(frame?.child?.items.map((item) => item.hovered)).toEqual([
      true,
      false,
    ]);
  });

  it('is null when no menu is open or the scale is unusable', () => {
    expect(menuFrameCss(null, [0, 0], 1)).toBeNull();
    expect(menuFrameCss(menu, [0, 0], Number.NaN)).toBeNull();
  });

  it('marks the row whose child list is open, and only that row', () => {
    // The invariant an earlier build spent `hovered` on and could not keep: the
    // parent went dark whenever the pointer crossed another top-level row,
    // leaving the open list with nothing pointing at it. `owner` is a fact
    // about the list, so it holds whatever the hover is doing -- and here the
    // hover is on row 1 while row 0 owns the list.
    const frame = menuFrameCss(withChild, [0, 0], 1);

    expect(frame?.items.map((item) => item.open)).toEqual([true, false]);
    expect(frame?.items.map((item) => item.hovered)).toEqual([false, true]);
  });

  it('marks no row open when no child list is open', () => {
    expect(
      menuFrameCss(menu, [0, 0], 1)?.items.every((item) => !item.open),
    ).toBe(true);
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

  it('arms Filter on F, in either case', () => {
    expect(armedTypeForKey(key({ key: 'f' }))).toBe('filter');
    // Roadmap 1.24. The review of that change deleted the `case 'u'` arm
    // outright and all 75 tests stayed green -- the entire user-facing entry
    // point of the feature, and the one line nothing could catch.
    expect(armedTypeForKey(key({ key: 'u' }))).toBe('upscale');
    expect(armedTypeForKey(key({ key: 'U' }))).toBe('upscale');
    expect(armedTypeForKey(key({ key: 'F' }))).toBe('filter');
  });

  it('keeps the armable types distinct from each other', () => {
    // The switch returned one value for every key it matched until Filter
    // arrived, so a fallthrough between the arms would have been invisible.
    // ALL THREE PAIRWISE since 1.24, not just the first two: comparing one
    // pair cannot see a third arm falling through into either of them.
    const armed = ['s', 'f', 'u'].map((k) => armedTypeForKey(key({ key: k })));
    expect(new Set(armed).size).toBe(armed.length);
    expect(armed.every((a) => a !== null)).toBe(true);
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

describe('frozenFrameKeys', () => {
  // The dev rig, converted at dpr 1 with a zero origin so the CSS rects are the
  // physical ones and the test reads as the desktop it describes.
  const rig = [
    { x: 0, y: 0, width: 2560, height: 1440 },
    { x: 2560, y: 0, width: 1920, height: 1080 },
    { x: 4480, y: 0, width: 1920, height: 1080 },
    { x: -1080, y: 0, width: 1080, height: 1920 },
  ];

  it('marks only the monitor the still covers', () => {
    // The defect this replaced: one still on a four-monitor desktop used to
    // print `frozen` on all four, because the badge asked `stills.length > 0`.
    const frozen = frozenFrameKeys([{ frame: rig[1] }]);
    expect(rig.map((frame) => frozen.has(frameKey(frame)))).toEqual([
      false,
      true,
      false,
      false,
    ]);
  });

  it('marks every monitor when the freeze covered the whole desktop', () => {
    // The positive control, and it is not decoration: without it a
    // `frozenFrameKeys` that always returned an empty set would pass the test
    // above. The widened setting (UPTAKE_FREEZE_ALL_MONITORS) produces this.
    const frozen = frozenFrameKeys(rig.map((frame) => ({ frame })));
    expect(rig.every((frame) => frozen.has(frameKey(frame)))).toBe(true);
  });

  it('marks nothing when the screen is live', () => {
    expect(frozenFrameKeys([]).size).toBe(0);
  });

  it('agrees with the key the monitor loop renders with', () => {
    // The two are the same function on purpose. If the component ever keys its
    // {#each} differently from this, every badge silently stops matching and
    // the screen reads as live while it is frozen -- the failure in the other
    // direction, which is the one no user would report as a bug.
    expect(frameKey(rig[2])).toBe('4480,0,1920,1080');
  });
});

describe('formatZoom', () => {
  it('prints a whole factor without decimals', () => {
    // The commonest values in the range. `2.00×` reads as an instrument
    // reporting a measurement; `2×` reads as the setting the user chose.
    expect(formatZoom(2)).toBe('2×');
    expect(formatZoom(8)).toBe('8×');
  });

  it('keeps the quarters the step actually produces', () => {
    expect(formatZoom(1.25)).toBe('1.25×');
    expect(formatZoom(1.5)).toBe('1.5×');
    expect(formatZoom(3.75)).toBe('3.75×');
  });

  it('survives the float the wire delivers', () => {
    // The factor is computed in Rust as an f32 and arrives as an IEEE double,
    // so it is not exactly the quarter it represents. A formatter that
    // stringified the number directly would print `2.4999999403953552×`.
    expect(formatZoom(2.4999999403953552)).toBe('2.5×');
    expect(formatZoom(0.9999999)).toBe('1×');
  });
});
