import { describe, expect, it } from 'vitest';

import {
  appearanceStyle,
  appearanceVars,
  OPACITY_REFERENCE,
} from './appearance';

/**
 * The appearance settings' arithmetic (roadmap 1.14).
 *
 * The assertion that earns this file is the first one: at the shipped defaults
 * every alpha the overlay draws must be the number that was already in
 * `+page.svelte`'s stylesheet. A settings row that quietly restyled the product
 * at its own default would be a redesign nobody approved, and it would look
 * like a feature in the diff.
 */

/** The alphas `.area` and `.area.filter` carried before 1.14 touched them. */
const SHIPPED = {
  border: 0.9,
  fill: 0.06,
  glow: 0.3,
  hoveredFill: 0.12,
  hoveredGlow: 0.5,
  filterFill: 0.16,
  filterHoveredFill: 0.24,
};

describe('the appearance variables', () => {
  it('leave the overlay exactly as it shipped at the default percentages', () => {
    const vars = appearanceVars(40, 16);
    const solidity = Number(vars['--area-solidity']);
    const filterAlpha = Number(vars['--filter-alpha']);

    // The multiplier is exactly 1, so every `calc(<shipped> * var(...))` in the
    // stylesheet evaluates to the shipped number.
    expect(solidity).toBe(1);
    expect(SHIPPED.border * solidity).toBe(SHIPPED.border);
    expect(SHIPPED.fill * solidity).toBe(SHIPPED.fill);
    expect(SHIPPED.glow * solidity).toBe(SHIPPED.glow);
    expect(SHIPPED.hoveredFill * solidity).toBe(SHIPPED.hoveredFill);
    expect(SHIPPED.hoveredGlow * solidity).toBe(SHIPPED.hoveredGlow);

    // The wash alpha is named directly rather than scaled, so its default is
    // the shipped fill and its hover is the 1.5x the stylesheet applies.
    expect(filterAlpha).toBe(SHIPPED.filterFill);
    expect(1.5 * filterAlpha).toBeCloseTo(SHIPPED.filterHoveredFill, 10);
  });

  it('anchors the scale at the documented reference, not at an arbitrary number', () => {
    // If someone changes the default in `settings.rs` without changing this,
    // the test above goes red rather than the product quietly restyling.
    expect(OPACITY_REFERENCE).toBe(40);
    expect(
      Number(appearanceVars(OPACITY_REFERENCE, 16)['--area-solidity']),
    ).toBe(1);
  });

  it('moves both ways from the default', () => {
    expect(Number(appearanceVars(20, 16)['--area-solidity'])).toBe(0.5);
    expect(Number(appearanceVars(80, 16)['--area-solidity'])).toBe(2);
    expect(Number(appearanceVars(40, 5)['--filter-alpha'])).toBe(0.05);
    expect(Number(appearanceVars(40, 60)['--filter-alpha'])).toBe(0.6);
  });

  it('lets the top of the slider reach a fully solid border', () => {
    // CSS clamps an alpha above 1, which is what makes 100% mean solid rather
    // than an invalid colour. The border is the highest shipped alpha, so it
    // is the one that has to clear 1 first.
    const solidity = Number(appearanceVars(100, 16)['--area-solidity']);
    expect(SHIPPED.border * solidity).toBeGreaterThanOrEqual(1);
  });

  it('writes a style attribute the browser will parse', () => {
    expect(appearanceStyle(40, 16)).toBe(
      '--area-solidity: 1; --filter-alpha: 0.16',
    );
  });
});
