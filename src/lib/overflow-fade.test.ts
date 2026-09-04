// @vitest-environment jsdom
//
// The default environment here is `node`, which suits every other `src/lib`
// suite because they test pure functions. This one drives a Svelte action, and
// an action needs an element to attach to. Same directive, for the same reason,
// as `src/routes/page.svelte.test.ts`.

import { describe, expect, it } from 'vitest';

import pageSvelte from '../routes/+page.svelte?raw';
import { overflowFade, overflows } from './overflow-fade';

/**
 * The rule this action exists to switch on must survive compilation.
 *
 * **This is the defect it was written for, caught in this branch before it
 * merged.** `overflowFade` sets `data-overflowing` at run time, so Svelte's CSS
 * pruner cannot see the attribute in the markup and drops
 * `.ocr[data-overflowing='true']::after` as unreachable. It says so as a
 * WARNING and the build succeeds, so the first version here shipped a fade that
 * could never render and every test was green.
 *
 * A text control rather than a rendered-CSS assertion, matching what
 * `menu-styles.test.ts` already does with this same file: the compiled
 * stylesheet is not reachable from a component test, and the property that
 * matters is a property of the source.
 */
describe('the fade rule survives Svelte CSS pruning', () => {
  it('guards the runtime attribute with :global', () => {
    expect(pageSvelte).toContain(":global([data-overflowing='true'])");
  });

  // The pruned form, named so a "simplification" back to it fails here rather
  // than silently removing the fade.
  it('does not use the bare attribute selector the compiler prunes', () => {
    expect(pageSvelte).not.toContain(".ocr[data-overflowing='true']::after");
  });

  // The action has to be attached, or the attribute is never set and the rule
  // above is correct and dead.
  it('attaches the action to the OCR panel', () => {
    expect(pageSvelte).toContain('use:overflowFade');
  });

  // `overflow: auto` is what I-353 is: a scrollbar over content the user cannot
  // reach, on an element that is `pointer-events: none` by ADR-0016.
  it('does not restore the scrollbar the panel cannot use', () => {
    const ocrRule = pageSvelte.slice(pageSvelte.indexOf('.ocr {'));
    const body = ocrRule.slice(0, ocrRule.indexOf('}'));
    expect(body).toContain('overflow: hidden');
    expect(body).not.toContain('overflow: auto');
  });
});

describe('overflows', () => {
  it('is false when the content fits', () => {
    expect(overflows(40, 40)).toBe(false);
    expect(overflows(12, 40)).toBe(false);
  });

  // The tolerance, asserted from both sides. A strict `>` would mark a
  // perfectly-fitting single line as truncated on some zoom levels, and a fade
  // under "No text found" teaches the user to ignore every fade.
  it('tolerates one pixel of sub-pixel rounding and no more', () => {
    expect(overflows(41, 40)).toBe(false);
    expect(overflows(42, 40)).toBe(true);
  });

  it('is true when the content is plainly taller', () => {
    expect(overflows(180, 40)).toBe(true);
  });
});

describe('overflowFade', () => {
  /**
   * A stand-in element. jsdom does no layout, so `scrollHeight` and
   * `clientHeight` are both 0 on a real node and the action could not be
   * driven through one -- the numbers are supplied here instead. That is the
   * whole reason the decision lives in `overflows` as a pure function: this
   * suite can check what the action WRITES, and `overflows` is what decides.
   */
  function node(scrollHeight: number, clientHeight: number): HTMLElement {
    const element = document.createElement('div');
    Object.defineProperty(element, 'scrollHeight', { value: scrollHeight });
    Object.defineProperty(element, 'clientHeight', { value: clientHeight });
    return element;
  }

  it('marks a truncated panel on attach', () => {
    const element = node(200, 40);
    const action = overflowFade(element);
    expect(element.dataset.overflowing).toBe('true');
    action.destroy();
  });

  // Written as "false" rather than left absent, so the CSS selector has a
  // value to match and a panel that stops overflowing loses its fade rather
  // than keeping a stale attribute.
  it('marks a fitting panel explicitly false rather than leaving it unset', () => {
    const element = node(30, 40);
    const action = overflowFade(element);
    expect(element.dataset.overflowing).toBe('false');
    action.destroy();
  });

  // The action's job is a visual hint. Taking the overlay down at import time
  // because an observer is missing would trade a missing fade for a blank
  // screen, so both observers are optional and `destroy` must survive their
  // absence.
  it('attaches and detaches where the observers do not exist', () => {
    const resize = globalThis.ResizeObserver;
    const mutation = globalThis.MutationObserver;
    // @ts-expect-error deliberately removing a global to drive the guard
    delete globalThis.ResizeObserver;
    // @ts-expect-error deliberately removing a global to drive the guard
    delete globalThis.MutationObserver;
    try {
      const element = node(200, 40);
      const action = overflowFade(element);
      expect(element.dataset.overflowing).toBe('true');
      expect(() => action.destroy()).not.toThrow();
    } finally {
      globalThis.ResizeObserver = resize;
      globalThis.MutationObserver = mutation;
    }
  });
});
