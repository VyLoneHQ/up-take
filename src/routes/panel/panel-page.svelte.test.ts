// @vitest-environment jsdom

/**
 * The area settings panel's page (roadmap 1.60), rendered.
 *
 * `I-479`, the founder's rig, 2026-11-03: the panel was cut off below its
 * watch buttons. Rust sizes the window from the height the page reports, and
 * starts every newly opened panel at a first guess; the page reported only
 * when its height CHANGED, and the page outlives the panel (its window is
 * hidden, not closed). So the second panel to open was never measured and
 * kept the guess. These tests drive the page the way Rust does, through
 * `panel://changed`, and read what it sends back.
 */

import { invoke } from '@tauri-apps/api/core';
import { cleanup, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  expect,
  test,
  vi,
} from 'vitest';

const handlers = new Map<string, () => void>();

vi.mock('@tauri-apps/api/event', () => ({
  listen: (name: string, handler: () => void) => {
    handlers.set(name, handler);
    return Promise.resolve(() => handlers.delete(name));
  },
}));

const replies = new Map<string, unknown>();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn((command: string) => Promise.resolve(replies.get(command))),
}));

const Page = (await import('./+page.svelte')).default;

/** jsdom lays nothing out: the panel's height is given here. */
const PANEL_HEIGHT = 200;
/** The page's margin around the panel, both sides (`MARGIN` in the page). */
const MARGINS = 24;

const original = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  'offsetHeight',
);

beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, 'offsetHeight', {
    configurable: true,
    get: () => PANEL_HEIGHT,
  });
  // jsdom has no ResizeObserver; the page's own report on mount is what is
  // under test, not the observer.
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

afterAll(() => {
  if (original)
    Object.defineProperty(HTMLElement.prototype, 'offsetHeight', original);
});

afterEach(() => cleanup());

beforeEach(() => {
  handlers.clear();
  replies.clear();
  vi.mocked(invoke).mockClear();
  replies.set('panel_view', WATCH);
});

/** A watch area, free, on the menu's default depth. */
const WATCH = {
  id: 1,
  kind: 'watch',
  name: null,
  watch: 'changes',
  held: false,
  tuning: {
    notify: 'each',
    calm_ms: 150,
    gap_ms: 0,
    sample_ms: 0,
    share_permille: 0,
    settle_ms: 1_000,
  },
  kinds: ['default', 'screenshot', 'filter', 'upscale', 'ocr', 'watch'],
  ocr: null,
  depth: 'auto',
  sticky: false,
  can_stick: true,
  on_resize: null,
  click_through: false,
  capture: false,
};

/** The last command the page sent other than its height. */
function lastSet() {
  return vi
    .mocked(invoke)
    .mock.calls.filter(
      ([command]) =>
        command !== 'panel_resize' && command !== 'panel_return_keyboard',
    )
    .at(-1);
}

function button(name: string): HTMLElement {
  const found = [...document.querySelectorAll<HTMLElement>('button')].find(
    (candidate) => candidate.textContent?.trim() === name,
  );
  if (!found) throw new Error(`no button "${name}"`);
  return found;
}

function switchNamed(name: string): HTMLElement {
  const found = [
    ...document.querySelectorAll<HTMLElement>('[role="switch"]'),
  ].find((candidate) => candidate.textContent?.trim() === name);
  if (!found) throw new Error(`no switch "${name}"`);
  return found;
}

// Roadmap 1.60, second slice: the menu's rows, in the panel he approved on
// 2026-11-05. Each control sends the command that runs the menu row's action.
test('each of the menu rows sends its own command', async () => {
  render(Page);
  await settle();
  button('Screenshot').click();
  expect(lastSet()).toEqual(['panel_set_type', { kind: 'screenshot' }]);
  button('Always on top').click();
  expect(lastSet()).toEqual(['panel_set_depth', { depth: 'front' }]);
  switchNamed('Stick to window').click();
  expect(lastSet()).toEqual(['panel_set_sticky', { sticky: true }]);
  switchNamed('Click-through').click();
  expect(lastSet()).toEqual(['panel_set_click_through', { on: true }]);
  // The panel offers no Dismiss: an area has its own close control, and
  // Ctrl+right-click keeps the menu that has one (his choice).
  expect(
    [...document.querySelectorAll('button')].some(
      (candidate) => candidate.textContent?.trim() === 'Dismiss',
    ),
  ).toBe(false);
});

// Roadmap 1.65, his choices of 2026-11-07: notifications (none, once, every
// time), a quiet time for "once", a minimum gap, and a sampling slider.
test("a watch area's tuning is sent whole with the one value changed", async () => {
  render(Page);
  await settle();
  const base = WATCH.tuning;
  expect(document.body.textContent).not.toContain('Quiet time before the next');
  button('Once').click();
  expect(lastSet()).toEqual([
    'panel_set_tuning',
    { tuning: { ...base, notify: 'once' } },
  ]);
  await settle();
  expect(document.body.textContent).toContain('Quiet time before the next');
  button('1 s').click();
  expect(lastSet()).toEqual([
    'panel_set_tuning',
    { tuning: { ...base, notify: 'once', gap_ms: 1_000 } },
  ]);
  const sliders = document.querySelectorAll<HTMLInputElement>(
    'input[type="range"]',
  );
  const sample = sliders[sliders.length - 1];
  expect(document.body.textContent).toContain('Realtime');
  sample.value = '0';
  sample.dispatchEvent(new Event('input', { bubbles: true }));
  expect(lastSet()).toEqual([
    'panel_set_tuning',
    {
      tuning: { ...base, notify: 'once', gap_ms: 1_000, sample_ms: 5_000 },
    },
  ]);
  await settle();
  expect(document.body.textContent).toContain('every 5 s');
});

test("a watch area's tolerance runs from any change to the whole area (1.61)", async () => {
  render(Page);
  await settle();
  const base = WATCH.tuning;
  expect(document.body.textContent).toContain('How much must change');
  expect(document.body.textContent).toContain('Any change');
  const share = document.querySelectorAll<HTMLInputElement>(
    'input[type="range"]',
  )[0];
  share.value = share.max;
  share.dispatchEvent(new Event('input', { bubbles: true }));
  expect(lastSet()).toEqual([
    'panel_set_tuning',
    { tuning: { ...base, share_permille: 1_000 } },
  ]);
  await settle();
  expect(document.body.textContent).toContain('100% of the area');
});

test('stops changing shows its own calm, and only for that choice', async () => {
  render(Page);
  await settle();
  expect(document.body.textContent).not.toContain('Counts as stopped after');
  replies.set('panel_view', { ...WATCH, watch: 'settles' });
  handlers.get('panel://changed')?.();
  await settle();
  expect(document.body.textContent).toContain('Counts as stopped after');
  expect(document.body.textContent).toContain('1 s');
});

test('every slider moves 1% a step, and 10% with Ctrl held', async () => {
  render(Page);
  await settle();
  const share = document.querySelectorAll<HTMLInputElement>(
    'input[type="range"]',
  )[0];
  expect(share.step).toBe('1');
  share.dispatchEvent(
    new PointerEvent('pointerdown', { bubbles: true, ctrlKey: true }),
  );
  await settle();
  expect(share.step).toBe('10');
  share.dispatchEvent(
    new PointerEvent('pointermove', { bubbles: true, ctrlKey: false }),
  );
  await settle();
  expect(share.step).toBe('1');
  // Ctrl let go on the keyboard, between two arrow keys (round 6).
  share.dispatchEvent(
    new KeyboardEvent('keydown', { bubbles: true, ctrlKey: true }),
  );
  await settle();
  expect(share.step).toBe('10');
  share.dispatchEvent(
    new KeyboardEvent('keyup', { bubbles: true, ctrlKey: false }),
  );
  await settle();
  expect(share.step).toBe('1');
  share.value = '37';
  share.dispatchEvent(new Event('input', { bubbles: true }));
  expect(lastSet()).toEqual([
    'panel_set_tuning',
    { tuning: { ...WATCH.tuning, share_permille: 370 } },
  ]);
  await settle();
  expect(document.body.textContent).toContain('37% of the area');
});

// Review of #168, round 9: every slider, the two that show only in some
// modes included, must take Ctrl itself. With "stops changing" and "Once"
// all four are on screen; each is given Ctrl on its own, and let go on its
// own, so a slider missing either handler leaves the step where it was.
test('each of the four sliders takes Ctrl, pressed and let go', async () => {
  replies.set('panel_view', {
    ...WATCH,
    watch: 'settles',
    tuning: { ...WATCH.tuning, notify: 'once' },
  });
  render(Page);
  await settle();
  const sliders = [
    ...document.querySelectorAll<HTMLInputElement>('input[type="range"]'),
  ];
  expect(sliders).toHaveLength(4);
  for (const [index, slider] of sliders.entries()) {
    expect(slider.max, `slider ${index}`).toBe('100');
    slider.dispatchEvent(
      new PointerEvent('pointerdown', { bubbles: true, ctrlKey: true }),
    );
    await settle();
    expect(slider.step, `slider ${index} with Ctrl`).toBe('10');
    slider.dispatchEvent(
      new KeyboardEvent('keyup', { bubbles: true, ctrlKey: false }),
    );
    await settle();
    expect(slider.step, `slider ${index} without Ctrl`).toBe('1');
  }
});

test('a type shows only its own rows', async () => {
  replies.set('panel_view', {
    ...WATCH,
    kind: 'screenshot',
    watch: null,
    held: null,
    tuning: null,
    sticky: true,
    on_resize: 'corner',
    capture: true,
  });
  render(Page);
  await settle();
  expect(document.body.textContent).not.toContain('Notify when it changes');
  expect(document.body.textContent).not.toContain('Pause');
  expect(document.body.textContent).not.toContain('Sampling rate');
  button('Keep top left').click();
  expect(lastSet()).toEqual(['panel_set_on_resize', { onResize: 'top_left' }]);
  button('Copy').click();
  expect(lastSet()).toEqual(['panel_export', { save: false }]);
  button('Save image').click();
  expect(lastSet()).toEqual(['panel_export', { save: true }]);
});

test('with no window to stick to, the switch does nothing', async () => {
  replies.set('panel_view', { ...WATCH, can_stick: false });
  render(Page);
  await settle();
  vi.mocked(invoke).mockClear();
  switchNamed('Stick to window').click();
  expect(lastSet()).toBeUndefined();
});

async function settle() {
  for (let i = 0; i < 4; i += 1) {
    await Promise.resolve();
    await tick();
  }
}

function resizes() {
  return vi
    .mocked(invoke)
    .mock.calls.filter(([command]) => command === 'panel_resize');
}

test('the first panel reports its height', async () => {
  render(Page);
  await settle();
  expect(resizes()).toEqual([
    ['panel_resize', { height: PANEL_HEIGHT + MARGINS }],
  ]);
});

test('a panel opened again reports its height again (I-479)', async () => {
  render(Page);
  await settle();
  vi.mocked(invoke).mockClear();
  // Rust closed the panel, opened it again at its first guess, and told the
  // page to read its area: the page has to say how tall it is.
  handlers.get('panel://changed')?.();
  await settle();
  expect(resizes()).toEqual([
    ['panel_resize', { height: PANEL_HEIGHT + MARGINS }],
  ]);
});

// Review of #161, round 1: a click outside closed the panel before the name
// field lost focus, and a name saved only then was dropped or could land on
// another area. It is saved as it is typed, by the area's own id.
test('a name is saved as it is typed, for its own area', async () => {
  render(Page);
  await settle();
  const field = document.querySelector('input') as HTMLInputElement;
  field.focus();
  field.value = 'Do';
  field.dispatchEvent(new Event('input', { bubbles: true }));
  await settle();
  expect(lastSet()).toEqual(['panel_set_name', { id: 1, name: 'Do' }]);
  field.value = 'Download';
  field.dispatchEvent(new Event('input', { bubbles: true }));
  await settle();
  expect(lastSet()).toEqual(['panel_set_name', { id: 1, name: 'Download' }]);
});

// Review of #161, round 1: a click on a panel button activated the panel, and
// placing went deaf to its keys. Each control hands the keyboard back; typing
// a name does not.
test('using a control gives the keyboard back, typing a name does not', async () => {
  render(Page);
  await settle();
  const returns = () =>
    vi
      .mocked(invoke)
      .mock.calls.filter(([command]) => command === 'panel_return_keyboard')
      .length;
  const field = document.querySelector('input') as HTMLInputElement;
  field.value = 'x';
  field.dispatchEvent(new Event('input', { bubbles: true }));
  await settle();
  expect(returns()).toBe(0);
  button('Always on top').click();
  expect(returns()).toBe(1);
  button('Notify when it stops changing').click();
  expect(returns()).toBe(2);
  switchNamed('Click-through').click();
  expect(returns()).toBe(3);
});
