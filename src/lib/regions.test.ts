import { describe, expect, it, vi } from 'vitest';

import {
  type CssRect,
  hideOverlay,
  type Invoke,
  isDismissKey,
  type Measurable,
  measure,
  reportInteractiveRegions,
} from './regions';

/** A stand-in for an element, so these tests need no DOM. */
function stub(rect: CssRect): Measurable {
  return { getBoundingClientRect: () => rect };
}

const PILL: CssRect = { x: 2085.0625, y: 32, width: 277.86, height: 32 };

describe('measure', () => {
  it('reports each element as a CSS rect', () => {
    expect(measure([stub(PILL)])).toEqual([PILL]);
  });

  it('skips elements that are not bound yet', () => {
    // `bind:this` is undefined until mount, and resize can fire first.
    expect(measure([null, stub(PILL), undefined])).toEqual([PILL]);
  });

  it('reports nothing for no elements', () => {
    expect(measure([])).toEqual([]);
  });
});

describe('reportInteractiveRegions', () => {
  it('sends the scale the rects were measured in', async () => {
    const invoke = vi.fn<Invoke>().mockResolvedValue(undefined);

    await reportInteractiveRegions(invoke, [stub(PILL)], 1.25);

    expect(invoke).toHaveBeenCalledWith('overlay_set_interactive_regions', {
      regions: [PILL],
      scale: 1.25,
    });
  });

  it('sends the scale it is given even when the window disagrees', async () => {
    // The regression this module exists for: the WebView laid out at 1.25 while
    // tao reported 1.0 for the same window. Whatever the window thinks, the
    // rects were measured at the ratio passed in and must travel with it.
    const invoke = vi.fn<Invoke>().mockResolvedValue(undefined);

    await reportInteractiveRegions(invoke, [stub(PILL)], 1.25);

    const [, args] = invoke.mock.calls[0];
    expect(args?.scale).toBe(1.25);
  });

  it('resolves false instead of throwing when the report fails', async () => {
    // An unhandled rejection here would leave the user behind a full-desktop
    // overlay; Rust treats an unreported region set as "stay interactive".
    const invoke = vi.fn<Invoke>().mockRejectedValue(new Error('ACL denied'));

    await expect(
      reportInteractiveRegions(invoke, [stub(PILL)], 1),
    ).resolves.toBe(false);
  });

  it('resolves true when the report lands', async () => {
    const invoke = vi.fn<Invoke>().mockResolvedValue(undefined);

    await expect(
      reportInteractiveRegions(invoke, [stub(PILL)], 1),
    ).resolves.toBe(true);
  });

  it('sends nothing when every element is still unbound', async () => {
    // A mount race must not overwrite a good region set with an empty one:
    // empty means "the whole window takes input" on the Rust side, so the
    // overlay would swallow every click desktop-wide until the next resize.
    const invoke = vi.fn<Invoke>().mockResolvedValue(undefined);

    await expect(reportInteractiveRegions(invoke, [null], 1)).resolves.toBe(
      false,
    );
    expect(invoke).not.toHaveBeenCalled();
  });

  it('still reports a genuinely empty region set', async () => {
    // Distinguished from the race above by the caller passing no elements:
    // that is a deliberate "nothing is interactive", not a missed measurement.
    const invoke = vi.fn<Invoke>().mockResolvedValue(undefined);

    await expect(reportInteractiveRegions(invoke, [], 1)).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith('overlay_set_interactive_regions', {
      regions: [],
      scale: 1,
    });
  });

  it('reports the elements that are bound when only some are', async () => {
    // The partial case task 1.6c makes ordinary: some areas mounted, some not.
    const invoke = vi.fn<Invoke>().mockResolvedValue(undefined);

    await reportInteractiveRegions(invoke, [null, stub(PILL)], 1);

    const [, args] = invoke.mock.calls[0];
    expect(args?.regions).toEqual([PILL]);
  });
});

describe('hideOverlay', () => {
  it('emits the hide intent', async () => {
    const invoke = vi.fn<Invoke>().mockResolvedValue(undefined);

    await expect(hideOverlay(invoke)).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith('overlay_hide');
  });

  it('resolves false instead of throwing when the intent fails', async () => {
    const invoke = vi.fn<Invoke>().mockRejectedValue(new Error('no window'));

    await expect(hideOverlay(invoke)).resolves.toBe(false);
  });
});

describe('isDismissKey', () => {
  it('accepts Escape', () => {
    expect(isDismissKey('Escape')).toBe(true);
  });

  it('rejects everything else', () => {
    for (const key of ['Esc', 'escape', 'Enter', ' ', 'a']) {
      expect(isDismissKey(key)).toBe(false);
    }
  });
});
