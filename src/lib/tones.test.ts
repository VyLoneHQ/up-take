import { describe, expect, it } from 'vitest';
import coach from './Coach.svelte?raw';
import { isTone, TONES, type Tone } from './tones';

describe('the colour legend (ADR-0054)', () => {
  it('matches the tour swatch of every type, so a card is the colour of its legend entry', () => {
    for (const tone of Object.keys(TONES) as Tone[]) {
      const block = coach.match(
        new RegExp(`\\.swatch\\.tone-${tone} \\{([^}]*)\\}`),
      )?.[1];
      expect(block, `swatch for ${tone}`).toBeDefined();
      expect(block).toContain(`rgba(${TONES[tone]},`);
    }
  });

  it('knows its tones and nothing else', () => {
    expect(isTone('watch')).toBe(true);
    expect(isTone('nope')).toBe(false);
    expect(isTone(3)).toBe(false);
  });
});
