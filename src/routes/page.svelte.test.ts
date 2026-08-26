// @vitest-environment jsdom

/**
 * The first component tests in this repository (UP-TAKE `I-23`).
 *
 * # What this closes
 *
 * `vitest` covered `src/lib/**` only, and nothing rendered a component. So
 * every `{#if}` in `+page.svelte` was unguarded, and this project's two worst
 * recent defects both lived on exactly that line:
 *
 * - **The frozen badge.** It asked `stills.length > 0` and rendered inside the
 *   per-monitor loop, so a freeze narrowed to the cursor's monitor would have
 *   printed *frozen* over three live screens. The fix moved the logic into
 *   `frozenFrameKeys`, which **is** tested. But a helper test proves the
 *   helper; it cannot prove the helper is CALLED, or called with the right
 *   argument. Rewriting the call site back to `frozenFrames.size > 0` restores
 *   the defect with the whole suite green. `I-23` says exactly this.
 * - **`F-38`'s broken-image pin**, which also passed a green suite.
 *
 * # The DOM environment is per file, not global
 *
 * The docblock at the top of this file is the whole of the ENVIRONMENT change,
 * so the existing DOM-free suite over `src/lib/**` keeps running in node at the
 * speed `I-23` names as a cost worth protecting. A project-wide `jsdom` would
 * have made every pure helper test pay for a DOM it never touches.
 *
 * ⚠️ **`vite.config.js` IS changed, and an earlier draft of this paragraph said
 * it was not.** One line, guarded on `process.env.VITEST`: Svelte has to
 * resolve its browser build or `render` dies with
 * `lifecycle_function_unavailable`. The claim being made here is narrower than
 * the one that was written: the DOM is per file, the module resolution is not
 * and cannot be. That file carries the reasoning.
 *
 * **What the split actually cost, measured rather than asserted**, since `I-23`
 * names the DOM-free suite's speed as the thing to protect:
 *
 * ```text
 *   src/lib alone   97 tests   751 ms wall   environment    1 ms
 *   whole suite    110 tests   3.6 s wall    environment 1.21 s
 * ```
 *
 * The `environment` column is the one the split was for: the pure helper tests
 * still pay **1 ms** for a DOM they never touch. The 2.9 s is jsdom starting up
 * and Svelte compiling an 800-line component, and it is paid by the file that
 * asked for it. A project-wide `jsdom` would have moved that cost onto all six
 * other files.
 *
 * # Why the tauri surface is mocked rather than stubbed at the boundary
 *
 * `+page.svelte` has no props. Every piece of its state arrives through
 * `listen()` from Rust, so a rendered component with nothing emitted into it is
 * a blank page and can assert nothing. {@link mount} captures the handlers the
 * component registers and {@link emit} calls them, which drives the component
 * **the way Rust drives it**: through the real payload shapes, on the real
 * event names. A test that reached inside the component instead would assert
 * against a structure this file is not allowed to know about (architecture §1:
 * Rust owns the state, this side renders it).
 *
 * # What this does NOT cover, counted rather than left to be assumed
 *
 * `+page.svelte` has **14** `{#if}` blocks and these **12** tests reach about
 * half of them. Untouched: the armed badge, the drag preview's `!area.source`
 * guard, the flash animation, `area.showClose`, the selection frame, and the
 * menu including its child list. **The harness is the deliverable here, not
 * coverage**, and nothing in this pull request claims otherwise, but a reader
 * arriving later should not read *"there are component tests now"* as *"the
 * template is guarded"*. Raised by the independent review of this pull request.
 *
 * ⚠️ **The event NAMES are duplicated here and nothing checks them against
 * Rust.** A rename on the Rust side leaves these tests green and testing
 * nothing, because a handler that is never registered is never called and the
 * assertions would run against a component in its initial state. That is a real
 * residue and it is `I-308`; `area-kinds.test.ts` is the shape that would fix
 * it, scraping the names out of `overlay.rs` the way it already scrapes the
 * type vocabulary. Not done here, because this file's job is to prove the
 * harness works.
 */

import { render } from '@testing-library/svelte';
import { tick } from 'svelte';
import { beforeEach, describe, expect, test, vi } from 'vitest';

/** Every handler the component registered, by event name. */
const handlers = new Map<string, (event: { payload: unknown }) => void>();

vi.mock('@tauri-apps/api/event', () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) => {
    handlers.set(name, handler);
    return Promise.resolve(() => handlers.delete(name));
  },
}));

// Resolves rather than rejects. The component calls `overlay_report_scale` on
// mount and swallows the failure on purpose (the endpoint is debug-only), so a
// rejecting mock would exercise a path every test would then be sharing with
// the thing it is actually asserting.
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(() => Promise.resolve()),
}));

const Page = (await import('./+page.svelte')).default;

/** A monitor rect on the wire: `[x, y, width, height]`, physical pixels. */
type Wire = [number, number, number, number];

/** The four-monitor dev rig, which is where the frozen-badge defect lives. */
const PRIMARY: Wire = [0, 0, 2560, 1440];
const SECOND: Wire = [2560, 0, 1920, 1080];
const THIRD: Wire = [-1080, 0, 1080, 1920];

/**
 * Renders the page and waits for its `onMount` to finish registering.
 *
 * The registration is asynchronous (`listen` returns a promise and the
 * component awaits them all), so a test that emitted immediately would emit
 * into an empty map and silently assert against a blank page. Two ticks: one
 * for the mount effect, one for the promise chain it starts.
 */
async function mount() {
  handlers.clear();
  const rendered = render(Page);
  await tick();
  await Promise.resolve();
  await tick();
  return rendered;
}

/** Delivers a payload the way Rust would, then lets Svelte re-render. */
async function emit(event: string, payload: unknown) {
  const handler = handlers.get(event);
  if (!handler) {
    // Loud rather than a no-op. A missing handler is the failure mode this
    // file's own header warns about, and swallowing it here would produce the
    // green-and-testing-nothing result described there.
    throw new Error(
      `nothing listened to ${event}; registered: ${[...handlers.keys()].join(', ')}`,
    );
  }
  handler({ payload });
  await tick();
}

/** A `overlay://state` payload with the fields a test does not care about. */
function state(overrides: Record<string, unknown> = {}) {
  return {
    state: 'placement',
    origin: [0, 0],
    monitors: [PRIMARY, SECOND, THIRD],
    armed: null,
    frozen: false,
    stills: [],
    freeze_probe: null,
    ...overrides,
  };
}

beforeEach(() => {
  handlers.clear();
  // jsdom reports 1; the component reads it on every state event and the
  // conversion is `physRectToCss`. Set explicitly so a jsdom default change
  // cannot quietly move every asserted rectangle.
  Object.defineProperty(window, 'devicePixelRatio', {
    value: 1,
    configurable: true,
  });
});

describe('the overlay renders what Rust tells it and nothing more', () => {
  test('a hidden overlay draws no monitor frames at all', async () => {
    const { container } = await mount();
    await emit('overlay://state', state({ state: 'hidden' }));
    expect(container.querySelectorAll('.monitor-frame')).toHaveLength(0);
  });

  test('placement draws one frame per monitor', async () => {
    const { container } = await mount();
    await emit('overlay://state', state());
    expect(container.querySelectorAll('.monitor-frame')).toHaveLength(3);
  });

  /**
   * 🔴 **The defect `I-23` was opened for, now guarded at the call site.**
   *
   * A freeze covers the cursor's monitor (ADR-0026's third amendment), so
   * exactly one badge may appear on a three-monitor desktop. The helper
   * `frozenFrameKeys` is unit-tested; this asserts the template CALLS it, which
   * is the half no helper test can reach. Rewrite the `{#if}` back to
   * `frozenFrames.size > 0` and this goes red while every other test stays
   * green.
   */
  test('the frozen badge appears over the frozen monitor and no other', async () => {
    const { container } = await mount();
    await emit(
      'overlay://state',
      state({
        frozen: true,
        stills: [[...PRIMARY, 'uptake-still://1.png']],
      }),
    );
    expect(container.querySelectorAll('.frozen-badge')).toHaveLength(1);
    // And the still itself is drawn, once. A badge with no image would be the
    // inverse claim: the screen labelled frozen while showing live content.
    expect(container.querySelectorAll('img.frozen-still')).toHaveLength(1);
  });

  test('a live screen carries no frozen badge and no still', async () => {
    const { container } = await mount();
    await emit('overlay://state', state());
    expect(container.querySelectorAll('.frozen-badge')).toHaveLength(0);
    expect(container.querySelectorAll('img.frozen-still')).toHaveLength(0);
  });

  test('two frozen monitors get two badges', async () => {
    // The negative half of the test above: it must count the stills rather
    // than cap at one, or a whole-desktop freeze would under-report.
    const { container } = await mount();
    await emit(
      'overlay://state',
      state({
        frozen: true,
        stills: [
          [...PRIMARY, 'uptake-still://1.png'],
          [...SECOND, 'uptake-still://2.png'],
        ],
      }),
    );
    expect(container.querySelectorAll('.frozen-badge')).toHaveLength(2);
  });
});

describe('an area renders the badges its state earns and no others', () => {
  /** One area on the wire, at natural zoom, with the fields tests vary. */
  function area(overrides: Record<string, unknown> = {}) {
    return {
      id: 1,
      rect: [100, 100, 400, 300],
      close: [492, 92, 18, 18],
      layer: 'auto',
      kind: 'default',
      zoom: 1,
      ...overrides,
    };
  }

  test('an area at natural size shows no zoom badge', async () => {
    const { container } = await mount();
    await emit('overlay://state', state());
    await emit('overlay://areas', { areas: [area()] });
    expect(container.querySelectorAll('.area')).toHaveLength(1);
    expect(container.querySelectorAll('.zoom-badge')).toHaveLength(0);
  });

  test('a magnified area reports its factor', async () => {
    const { container } = await mount();
    await emit('overlay://state', state());
    await emit('overlay://areas', { areas: [area({ zoom: 2 })] });
    const badge = container.querySelector('.zoom-badge');
    expect(badge?.textContent).toBe('2×');
  });

  /**
   * 🔴 **THE `I-289` TEST THAT WAS HERE IS DELETED, AND THE REASON IS THE ONE
   * THING THIS FILE MOST NEEDED TO GET RIGHT.**
   *
   * It read:
   *
   * ```ts
   * test('an upscale area shows no zoom badge, because it does not magnify', ...)
   *   await emit('overlay://areas', { areas: [area({ kind: 'upscale' })] });
   *   expect(container.querySelectorAll('.zoom-badge')).toHaveLength(0);
   * ```
   *
   * …with a doc comment claiming `I-289` was dissolved because *"ADR-0031 makes
   * `Upscale` natural, so the badge cannot render"*, and that the test would
   * fail if `default_zoom` ever handed `Upscale` a factor again.
   *
   * **Every part of that was false on this branch.** `AreaType::default_zoom`
   * returns `Zoom::UPSCALE` for `Upscale` right now (`area.rs:297` at this
   * PR's base), whose factor is `2.0`, and `overlay.rs:799` puts it straight on
   * the wire. ADR-0031's change ships on a **separate, unmerged branch**. So
   * `I-289`'s actual defect, a permanent `2×` badge on every `Upscale` area
   * advertising a gesture `zoom_by` refuses in both directions, **is live in
   * the code this PR is based on.**
   *
   * The test could not see it. {@link area} defaults `zoom: 1` and **nothing in
   * it derives `zoom` from `kind`**; that relationship lives only in the real
   * `AreaType::default_zoom`, which a mock payload never consults. So the test
   * asserted *"a payload with `zoom: 1` shows no badge"*: trivially true,
   * already covered by `an area at natural size shows no zoom badge` two tests
   * above, and unable to fail for the payload Rust actually sends. Passing
   * `zoom: 2`, the real value, makes it fail.
   *
   * **A green that could not have been earned, on the one row this file cited
   * as closed.** Found by the independent review of this pull request; the
   * author had written the test for a world that exists on a different branch.
   *
   * ⚠️ **The test is owed and is NOT recreated here**, because on this base the
   * honest version of it would assert the defect rather than the fix. It
   * belongs with ADR-0031's change, where it is true. Recorded as `I-309` so it
   * is a tracked row rather than something a session has to remember.
   * `OS-F46` is this workspace's measurement of what happens to the second
   * kind.
   *
   * **The general lesson, which outlives `I-289`:** a mock payload can assert
   * only what the mock encodes. Every relationship this file wants to rely on
   * between two fields of an `AreaView` is a relationship Rust owns and this
   * file has to be told. `I-308` is the same gap for the event names.
   */

  test('a filter area is marked as one, so the tint rule can reach it', async () => {
    const { container } = await mount();
    await emit('overlay://state', state());
    await emit('overlay://areas', { areas: [area({ kind: 'filter' })] });
    expect(container.querySelector('.area')?.classList).toContain('filter');
  });

  test('a pinned area shows its tier arrow, an auto one shows none', async () => {
    const { container } = await mount();
    await emit('overlay://state', state());
    await emit('overlay://areas', { areas: [area({ layer: 'front' })] });
    expect(container.querySelector('.layer-badge')?.textContent).toBe('▲');

    await emit('overlay://areas', { areas: [area({ layer: 'auto' })] });
    expect(container.querySelectorAll('.layer-badge')).toHaveLength(0);
  });

  /**
   * `F-38`'s class: the pin element renders only when there are pixels to put
   * in it. An `<img>` with an empty `src` is the broken-image icon, which is
   * what that finding put on screen.
   */
  test('an area with no pinned capture renders no image', async () => {
    const { container } = await mount();
    await emit('overlay://state', state());
    await emit('overlay://areas', { areas: [area({ kind: 'screenshot' })] });
    expect(container.querySelectorAll('img.pin')).toHaveLength(0);
  });

  test('a pinned capture renders at the url Rust announced', async () => {
    const { container } = await mount();
    await emit('overlay://state', state());
    await emit('overlay://areas', { areas: [area({ kind: 'screenshot' })] });
    await emit('overlay://pin', { id: 1, url: 'uptake-area://1-7.png' });
    const pin = container.querySelector('img.pin');
    expect(pin?.getAttribute('src')).toBe('uptake-area://1-7.png');
  });

  test('an unpin removes the image rather than blanking its src', async () => {
    // The distinction is the defect: an `<img>` left in the DOM with a cleared
    // `src` is a broken-image icon over the user's screen, which is `F-38`.
    const { container } = await mount();
    await emit('overlay://state', state());
    await emit('overlay://areas', { areas: [area({ kind: 'screenshot' })] });
    await emit('overlay://pin', { id: 1, url: 'uptake-area://1-7.png' });
    expect(container.querySelectorAll('img.pin')).toHaveLength(1);

    await emit('overlay://pin', { id: 1, url: null });
    expect(container.querySelectorAll('img.pin')).toHaveLength(0);
  });
});
