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
 *   whole suite    109 tests   3.6 s wall    environment 1.21 s
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
 * `+page.svelte` has **13** `{#if}` blocks and these **12** tests reach about
 * half of them. Untouched: the armed badge, the drag preview's `!area.source`
 * guard, the flash animation, `area.showClose`, the selection frame, and the
 * menu including its child list. **The harness is the deliverable here, not
 * coverage**, and nothing in this pull request claims otherwise, but a reader
 * arriving later should not read *"there are component tests now"* as *"the
 * template is guarded"*. Raised by the independent review of this pull request.
 *
 * ⚠️ **This said 14 until round 2 of that review, and how it was wrong is
 * worth more than the number.** It came from `grep -c '{#if'`, which counts a
 * **doc comment at line 443 quoting `{#if !area.source}` in prose** as a
 * fourteenth template block. Counting closing `{/if}` tags gives 13, and 13 is
 * right. A count meant to stop a reader over-trusting this file's coverage was
 * itself produced by a measurement that did not check what it was matching.
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
import type { AreaKind } from '$lib/overlay-state';
import areaRs from '../../crates/uptake-core/src/area.rs?raw';

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

/**
 * Whether Rust's `AreaType::default_zoom` gives this kind a MAGNIFIED zoom.
 *
 * # Why this exists (UP-TAKE `I-309`)
 *
 * The `I-289` test that used to live in this file asserted *"an upscale area
 * shows no zoom badge"* by passing `{ kind: 'upscale' }` to a fixture that
 * defaults `zoom: 1`. **Nothing in the fixture derived `zoom` from `kind`**, so
 * it asserted *"a payload with `zoom: 1` shows no badge"*: trivially true,
 * already covered two tests above, and unable to fail for the payload Rust
 * actually sends. It was deleted rather than left green, and `I-309`'s durable
 * half is the sentence this function answers:
 *
 * > A mock payload can assert only what the mock encodes. Every relationship
 * > this file wants to rely on between two fields of an `AreaView` is a
 * > relationship **Rust owns** and this file has to be told.
 *
 * So it is told, by reading the relationship out of Rust rather than by
 * restating it. Same technique as `area-kinds.test.ts` and
 * `built-type-count.test.ts`, and the same reason: a hand-copied answer is a
 * second source that drifts silently.
 *
 * # It answers *magnified or not*, not *how far*
 *
 * `Zoom::UPSCALE` was **deleted** by roadmap 1.29 (ADR-0031), so every arm reads
 * `Zoom::NATURAL` today and there is no factor in the source to read. A scraper
 * that tried to compute one would be parsing a constant that does not exist,
 * which is more machinery for less truth. *Born magnified* is the whole of what
 * the badge condition (`area.zoom > 1`) turns on.
 *
 * # Why `source` is a parameter
 *
 * So the scraper itself can be put under test. Reading only the real `area.rs`
 * would leave it with **no positive case**: every arm there says
 * `Zoom::NATURAL`, so nothing could tell a working scraper from one hardwired
 * to `false`, and a sweep asserting `false` for all seven kinds passes either
 * way. That is `UT-F-83` in miniature -- an assertion whose fixture cannot
 * distinguish the two hypotheses -- and it is caught here rather than shipped
 * because this file's own history is what the row is about.
 *
 * # It throws rather than answering when it cannot find the arm
 *
 * An unfound match must not read as *natural*. That is the direction that fails
 * silently: the defect this guards is a type quietly gaining a magnified
 * default, and a scraper that answered `false` for a renamed function would
 * report exactly that as fine. `built-type-count.test.ts` carries the same rule
 * in its own words.
 */
function bornMagnified(kind: AreaKind, source: string = areaRs): boolean {
  const start = source.indexOf('pub const fn default_zoom(');
  if (start === -1) {
    throw new Error(
      'could not find `pub const fn default_zoom(` in area.rs: has it been ' +
        'renamed or moved? An unfound function must not read as "natural".',
    );
  }
  const end = source.indexOf('\n    }\n', start);
  if (end === -1) throw new Error('could not find the end of `default_zoom`');

  // Comment lines are dropped before parsing: a doc comment inside the match may
  // quote an arm, and `payload-keys.test.ts` shipped that exact false positive.
  const body = source
    .slice(start, end)
    .split('\n')
    .filter((line) => !line.trimStart().startsWith('//'))
    .join('\n');

  // One arm: its `|`-separated variants on the left, its value on the right.
  // Written as one regex over the whole match rather than as a hand-rolled
  // splitter, because the arms ARE `|`-grouped today (all seven variants share a
  // single `=> Zoom::NATURAL`) and a per-line reading would find no variant on
  // the line that carries the value.
  const arms = body.matchAll(/((?:\s*\|?\s*Self::\w+)+)\s*=>\s*([^,\n]+)/g);
  for (const [, left, value] of arms) {
    const variants = (left ?? '').match(/Self::(\w+)/g) ?? [];
    for (const variant of variants) {
      if (variant.slice('Self::'.length).toLowerCase() === kind) {
        return (value ?? '').trim() !== 'Zoom::NATURAL';
      }
    }
  }
  throw new Error(
    `no \`default_zoom\` arm found for area kind "${kind}". The match has been ` +
      'reshaped and this scraper can no longer read it; fix the scraper rather ' +
      'than deleting the test it serves.',
  );
}

/** Every area kind on the wire, in `AreaKind`'s own order. */
const KINDS = [
  'default',
  'screenshot',
  'record',
  'ocr',
  'upscale',
  'analysis',
  'filter',
] as const satisfies readonly AreaKind[];

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
  /**
   * One area on the wire, with `zoom` DERIVED FROM `kind` the way Rust derives
   * it (UP-TAKE `I-309`).
   *
   * The literal `zoom: 1` this replaced is what made the deleted `I-289` test
   * unable to fail: it fixed the very field the assertion turned on, so
   * `{ kind: 'upscale' }` changed nothing about the payload and the test could
   * not tell a magnified Upscale area from a natural one. `zoom` is now read
   * from {@link bornMagnified}, so a kind that gains a magnified default in
   * Rust arrives here magnified too.
   *
   * An explicit `zoom` in `overrides` still wins, and that is deliberate rather
   * than an oversight: `a magnified area reports its factor` needs to send a
   * factor no type is born with, and it is asserting the BADGE, not the
   * kind-to-zoom relationship.
   */
  function area(overrides: Record<string, unknown> = {}) {
    const kind = (overrides.kind as AreaKind) ?? 'default';
    return {
      id: 1,
      rect: [100, 100, 400, 300],
      close: [492, 92, 18, 18],
      layer: 'auto',
      kind,
      // 2 rather than some larger number because the badge turns on `> 1`; the
      // value only has to be magnified, and inventing a bigger one would imply
      // this file knows a factor Rust no longer defines.
      zoom: bornMagnified(kind) ? 2 : 1,
      // 400x300, so above `CHROME_INSIDE_SPAN` on both axes: a grab bar flush
      // above the top edge, and NO outside handles, because an area this size
      // resizes from the bands drawn on its own border (ADR-0028 D1/D4).
      bar: [100, 82, 400, 18],
      handles: [],
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

  test('an upscale area shows no zoom badge, because Rust does not magnify it', async () => {
    // 🟢 **`I-309` DISCHARGED: the test the tombstone above says is owed.**
    //
    // The difference from the deleted version is one line in {@link area} and it
    // is the whole difference: `zoom` is no longer a literal, it is read out of
    // `AreaType::default_zoom`. So this asserts the thing it names -- *an
    // Upscale area, at the zoom Rust gives it, shows no badge* -- rather than
    // *a payload with `zoom: 1` shows no badge*, which was true of every
    // payload and is why the old one could not fail.
    //
    // **Confirmed able to go red before being trusted**: pointing
    // `default_zoom`'s `Upscale` arm at a magnified `Zoom` makes `bornMagnified`
    // answer true, the fixture sends a magnified area, the badge renders, and
    // this fails. That is `I-289`'s actual defect -- a permanent `2×` badge
    // advertising a scroll gesture `zoom_by` refuses in both directions -- and
    // it is now guarded on the frontend rather than only in Rust.
    const { container } = await mount();
    await emit('overlay://state', state());
    await emit('overlay://areas', { areas: [area({ kind: 'upscale' })] });

    expect(container.querySelectorAll('.area')).toHaveLength(1);
    expect(container.querySelectorAll('.zoom-badge')).toHaveLength(0);
  });

  test('no area type is born magnified, which is what the badge rule assumes', () => {
    // Rust's own `every_type_is_born_natural` asserts this about the same
    // function; this asserts that THIS FILE'S READING of it agrees. On its own
    // it is a weak test, and the two below are what make it worth having --
    // see the note on `source` in `bornMagnified`.
    for (const kind of KINDS) {
      expect(bornMagnified(kind), `${kind} is born magnified`).toBe(false);
    }
  });

  test('the scraper can actually SEE a magnified arm, not just report false', () => {
    // 🔴 **THE POSITIVE CONTROL, and without it everything above is unearned.**
    // The sweep asserts `false` for all seven kinds, so a `bornMagnified`
    // hardwired to `return false` passes it, passes the Upscale badge test, and
    // guards nothing -- while reading as two green tests about magnification.
    //
    // The real `area.rs` cannot supply this case: every arm there says
    // `Zoom::NATURAL`, which is the fix working, so the only way to feed the
    // scraper a magnified arm is to hand it one.
    const magnified = [
      '    pub const fn default_zoom(self) -> Zoom {',
      '        match self {',
      '            Self::Default',
      '            | Self::Screenshot => Zoom::NATURAL,',
      '            Self::Upscale => Zoom::NATURAL.zoomed_in(4),',
      '        }',
      '    }',
      '',
    ].join('\n');

    expect(bornMagnified('upscale', magnified)).toBe(true);
    expect(bornMagnified('default', magnified)).toBe(false);
  });

  test('a renamed or reshaped default_zoom throws instead of answering "natural"', () => {
    // The direction that fails SILENTLY. The defect being guarded is a type
    // quietly gaining a magnified default; a scraper that answered `false` for
    // a function it could not find would report exactly that as fine, and every
    // test above would stay green through a rename.
    expect(() => bornMagnified('upscale', 'fn something_else() {}\n')).toThrow(
      /default_zoom/,
    );
    // Found, but with no arm for this kind: also a throw, not a `false`.
    const partial = [
      '    pub const fn default_zoom(self) -> Zoom {',
      '        match self {',
      '            Self::Default => Zoom::NATURAL,',
      '        }',
      '    }',
      '',
    ].join('\n');
    expect(() => bornMagnified('upscale', partial)).toThrow(
      /no `default_zoom` arm/,
    );
  });

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

/**
 * Roadmap 1.17(b2)'s frontend half, which is the half
 * [ADR-0028](../../Projects/UP-TAKE/DECISIONS/ADR-0028-grabbable-chrome-for-a-pass-through-area.md)
 * names as the risk in as many words: *"the bar is a **frontend** surface ...
 * either `I-23` gets a harness first, or this build's frontend half is verified
 * on the rig by hand"*. `I-23` landed, so these are that harness being used for
 * the thing it was asked for.
 *
 * `UT-F-55` is the worked example the ADR cites -- a change whose Rust half was
 * correct with seven unit tests and four confirmed mutations, and whose Svelte
 * half printed *frozen* over three live screens. Every test below was drilled by
 * mutating the template and confirmed to go red.
 */
describe('an area outside chrome appears with its hover and not otherwise', () => {
  /**
   * This block's own area fixture. `zoom` is derived from `kind` for the same
   * reason the fixture above derives it (`I-309`), and it is spelled out here
   * rather than left at a literal because a **second** fixture in the same file
   * quietly hardcoding the one field `I-309` removed is `UT-F-76`'s shape: five
   * enumerations of one class, each written by its author, each coming back
   * short.
   *
   * It changes nothing today, because every area this block builds is
   * `default` or `filter` and both are born natural. That is exactly why it
   * would have survived unnoticed until the first test here passed
   * `kind: 'upscale'`.
   */
  function area(overrides: Record<string, unknown> = {}) {
    const kind = (overrides.kind as AreaKind) ?? 'default';
    return {
      id: 1,
      rect: [100, 100, 400, 300],
      close: [492, 92, 18, 18],
      layer: 'auto',
      kind,
      zoom: bornMagnified(kind) ? 2 : 1,
      bar: [100, 82, 400, 18],
      handles: [],
      ...overrides,
    };
  }

  /** A 20x20 area: below `CHROME_INSIDE_SPAN`, so Rust sends four handles. */
  function smallArea(overrides: Record<string, unknown> = {}) {
    return area({
      rect: [300, 300, 20, 20],
      close: [319, 283, 18, 18],
      // Clearing the north handle, which is what stops the two overlapping.
      bar: [300, 264, 20, 18],
      handles: [
        [301, 282, 18, 18],
        [301, 320, 18, 18],
        [282, 301, 18, 18],
        [320, 301, 18, 18],
      ],
      ...overrides,
    });
  }

  /** Tells the component the pointer is on this area. */
  async function hover(id: number | null, chromeOnly = false) {
    await emit('overlay://hover', { id, chromeOnly });
  }

  test('no bar is drawn until the pointer is on the area', async () => {
    // The whole of D2: hidden until hovered. A bar that drew unconditionally
    // would be a permanent 18 px strip above every area on the user's screen,
    // which is the objection D1 records against putting it inside.
    const { container } = await mount();
    await emit('overlay://state', state({ state: 'living' }));
    await emit('overlay://areas', { areas: [area()] });
    expect(container.querySelectorAll('.grab-bar')).toHaveLength(0);

    await hover(1);
    expect(container.querySelectorAll('.grab-bar')).toHaveLength(1);
  });

  test('the bar is placed at the rectangle Rust sent, not at one computed here', async () => {
    // The silent failure `AreaView.bar` warns about. A bar laid out on this side
    // would still be *visible*, so nothing looks broken; the area would simply
    // refuse to move, because the hook hit-tests Rust's rectangle and the user
    // is aiming at this one.
    const { container } = await mount();
    await emit('overlay://state', state({ state: 'living' }));
    await emit('overlay://areas', { areas: [area()] });
    await hover(1);

    const bar = container.querySelector('.grab-bar') as HTMLElement;
    // dpr is 1 and the origin is [0, 0] in `state()`, so CSS px equal physical.
    expect(bar.style.transform).toBe('translate3d(100px, 82px, 0)');
    expect(bar.style.width).toBe('400px');
    expect(bar.style.height).toBe('18px');
  });

  test('the bar follows Rust when it has flipped below the area', async () => {
    // 🔴 **THE TEST ABOVE COULD NOT FAIL WITHOUT THIS ONE, and the drill is what
    // said so.** Rewriting the template to lay the bar out here --
    // `translate3d(area.rect.x, area.rect.y - 18)` -- left it GREEN, because
    // that fixture's area sits well inside the monitor, so Rust's answer and the
    // naive recomputation are the same rectangle. A test whose fixture cannot
    // distinguish the two hypotheses is not testing between them.
    //
    // D1's flip is what separates them: an area at the top of a monitor has no
    // room above, so its bar goes BELOW. A frontend that assumed *above* would
    // draw the bar over the area's own content and 18 px away from the rectangle
    // the hook tests -- visible, wrong, and silent.
    const { container } = await mount();
    await emit('overlay://state', state({ state: 'living' }));
    await emit('overlay://areas', {
      areas: [area({ rect: [100, 0, 400, 300], bar: [100, 300, 400, 18] })],
    });
    await hover(1);

    const bar = container.querySelector('.grab-bar') as HTMLElement;
    expect(bar.style.transform).toBe('translate3d(100px, 300px, 0)');
  });

  test('the bar says what type the area is', async () => {
    // D3: a label and nothing clickable. The words are 1.18's to settle; that
    // the bar carries the AREA'S type rather than a fixed string is not.
    const { container } = await mount();
    await emit('overlay://state', state({ state: 'living' }));
    await emit('overlay://areas', { areas: [area({ id: 1, kind: 'filter' })] });
    await hover(1);

    expect(container.querySelector('.grab-bar-label')?.textContent).toBe(
      'Filter',
    );
  });

  test('an area whose bar fits nowhere draws no bar and is otherwise unharmed', async () => {
    // `bar: null` is a real state -- neither placement lands on a screen -- and
    // it must degrade to the pre-1.17(b2) behaviour rather than to a blank
    // rectangle at NaN, which would be chrome that hides what is under it while
    // being ungrabbable.
    const { container } = await mount();
    await emit('overlay://state', state({ state: 'living' }));
    await emit('overlay://areas', { areas: [area({ bar: null })] });
    await hover(1);

    expect(container.querySelectorAll('.grab-bar')).toHaveLength(0);
    expect(container.querySelectorAll('.area')).toHaveLength(1);
    expect(container.querySelectorAll('.close')).toHaveLength(1);
  });

  test('a large area draws no outside handles, a small one draws four', async () => {
    // D4's threshold, from the frontend's side. An empty `handles` list means
    // *this area resizes from the bands on its own border*, not *this area
    // cannot be resized*, so drawing four blocks around every large area would
    // be chrome asserting something false.
    const { container } = await mount();
    await emit('overlay://state', state({ state: 'living' }));
    await emit('overlay://areas', { areas: [area()] });
    await hover(1);
    expect(container.querySelectorAll('.outside-handle')).toHaveLength(0);

    await emit('overlay://areas', { areas: [smallArea()] });
    await hover(1);
    expect(container.querySelectorAll('.outside-handle')).toHaveLength(4);
  });

  test('the outside handles are placed where Rust put them', async () => {
    const { container } = await mount();
    await emit('overlay://state', state({ state: 'living' }));
    await emit('overlay://areas', { areas: [smallArea()] });
    await hover(1);

    const placed = [...container.querySelectorAll('.outside-handle')].map(
      (node) => (node as HTMLElement).style.transform,
    );
    expect(placed).toEqual([
      'translate3d(301px, 282px, 0)',
      'translate3d(301px, 320px, 0)',
      'translate3d(282px, 301px, 0)',
      'translate3d(320px, 301px, 0)',
    ]);
  });

  test('the whole outside surface leaves together when the hover does', async () => {
    // One surface as far as the user is concerned. A hover-out that removed the
    // bar and left four handles floating beside an area would read as chrome
    // failing to draw, which is why `showBar` is deliberately the same condition
    // as `showClose` rather than a second rule.
    const { container } = await mount();
    await emit('overlay://state', state({ state: 'living' }));
    await emit('overlay://areas', { areas: [smallArea()] });
    await hover(1);
    expect(container.querySelectorAll('.grab-bar')).toHaveLength(1);
    expect(container.querySelectorAll('.outside-handle')).toHaveLength(4);

    await hover(null);
    expect(container.querySelectorAll('.grab-bar')).toHaveLength(0);
    expect(container.querySelectorAll('.outside-handle')).toHaveLength(0);
    expect(container.querySelectorAll('.close')).toHaveLength(0);
  });

  test('a pass-through area the cursor is merely inside still gets its bar', async () => {
    // `chromeOnly` withholds the grab HIGHLIGHT, because a press on a
    // pass-through body goes to the app underneath. It must not withhold the
    // bar: the bar is the one surface on such an area that a press DOES act on,
    // so hiding it there would hide the feature exactly where it is needed.
    const { container } = await mount();
    await emit('overlay://state', state({ state: 'living' }));
    await emit('overlay://areas', { areas: [area({ kind: 'filter' })] });
    await hover(1, true);

    expect(container.querySelectorAll('.grab-bar')).toHaveLength(1);
    expect(container.querySelectorAll('.area.hovered')).toHaveLength(0);
  });
});
