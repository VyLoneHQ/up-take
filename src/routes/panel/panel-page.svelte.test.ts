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
import { render } from '@testing-library/svelte';
import { tick } from 'svelte';
import { afterAll, beforeAll, beforeEach, expect, test, vi } from 'vitest';

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

beforeEach(() => {
  handlers.clear();
  replies.clear();
  vi.mocked(invoke).mockClear();
  replies.set('panel_view', {
    id: 1,
    kind: 'watch',
    name: null,
    watch: 'changes',
    held: false,
  });
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
