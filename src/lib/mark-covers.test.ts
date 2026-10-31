import { describe, expect, it } from 'vitest';
import page from '../routes/+page.svelte?raw';
import { COVERS } from './mark-check';

/**
 * Not drawn over a mark, each for a reason: the mark itself, or a part that
 * sits inside an element already in COVERS (which covers wherever its parts
 * do).
 */
const EXEMPT: Record<string, string> = {
  'watch-badge': 'the mark itself',
  'watch-dot': 'the mark itself',
  pin: "inside its own .area, which covers for a later area's pin",
  'grab-bar-label': 'inside .grab-bar',
  'menu-item': 'inside .menu',
  label: 'inside .menu',
  arrow: 'inside .menu',
  tick: 'inside .menu',
  'ocr-band': 'inside .ocr',
  'ocr-handle': 'inside .ocr',
  'ocr-word': 'inside .ocr',
};

/** Everything the page draws from the areas onwards, by class. */
function drawnFromTheAreasOn(): Set<string> {
  const start = page.indexOf('{#each areaFrames as area');
  const end = page.indexOf('<style');
  expect(start).toBeGreaterThan(0);
  const markup = page.slice(start, end);
  const names = new Set(
    [...markup.matchAll(/class="([a-z-]+)/g)].map((match) => match[1]),
  );
  // A component drawn there is named by its root class.
  if (markup.includes('<Coach')) names.add('coach');
  return names;
}

describe('the cover list is the page markup, not a memory (#154 round 25)', () => {
  it('names every class drawn from the areas on, or exempts it with a reason', () => {
    const missing = [...drawnFromTheAreasOn()].filter(
      (name) => !COVERS.includes(name) && !(name in EXEMPT),
    );
    expect(missing).toEqual([]);
  });

  it('names nothing the page does not draw there', () => {
    const drawn = drawnFromTheAreasOn();
    expect(COVERS.filter((name) => !drawn.has(name))).toEqual([]);
    expect(Object.keys(EXEMPT).filter((name) => !drawn.has(name))).toEqual([]);
  });
});
