import { describe, expect, it } from 'vitest';

import { cssRectToPhys, type PhysRect, physRectToCss } from './overlay-state';

/**
 * The coach's layout report converts CSS back to physical pixels (roadmap
 * 1.18), and Rust hit-tests presses against the result. A conversion that
 * drifted from `physRectToCss` would put the coach's buttons somewhere other
 * than where they are drawn, silently: the panel would look right and refuse
 * every click.
 */
describe('cssRectToPhys is the inverse of physRectToCss', () => {
  const cases: {
    what: string;
    rect: PhysRect;
    origin: [number, number];
    dpr: number;
  }[] = [
    {
      what: 'the primary at 100%',
      rect: [100, 200, 500, 300],
      origin: [0, 0],
      dpr: 1,
    },
    {
      what: 'a monitor left of the primary',
      rect: [-1000, 40, 480, 260],
      origin: [-1080, 0],
      dpr: 1,
    },
    {
      what: 'a 125% overlay',
      rect: [250, 125, 625, 375],
      origin: [0, 0],
      dpr: 1.25,
    },
    {
      what: 'a 150% overlay with a negative origin',
      rect: [-600, 300, 900, 450],
      origin: [-1920, -120],
      dpr: 1.5,
    },
  ];

  it.each(cases)('round-trips $what', ({ rect, origin, dpr }) => {
    const css = physRectToCss(rect, origin, dpr);
    expect(css).not.toBeNull();
    if (css === null) return;
    expect(cssRectToPhys(css, origin, dpr)).toEqual(rect);
  });

  it('rounds outward, so a target is never smaller than it is drawn', () => {
    // 10.4 to 20.6 CSS px at dpr 1 covers physical 10 to 21.
    expect(
      cssRectToPhys({ x: 10.4, y: 10.4, width: 10.2, height: 10.2 }, [0, 0], 1),
    ).toEqual([10, 10, 11, 11]);
  });

  it('refuses a scale the forward conversion also refuses', () => {
    const rect = { x: 0, y: 0, width: 10, height: 10 };
    expect(cssRectToPhys(rect, [0, 0], 0)).toBeNull();
    expect(cssRectToPhys(rect, [0, 0], Number.NaN)).toBeNull();
  });
});
