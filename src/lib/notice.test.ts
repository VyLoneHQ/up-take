import { describe, expect, it } from 'vitest';
import { FADE_MS, isNotice, LINGER_MS } from './notice';

describe('the notification card (ADR-0054)', () => {
  it('lingers six seconds before it fades, as decided', () => {
    expect(LINGER_MS).toBe(6000);
    expect(FADE_MS).toBeLessThan(LINGER_MS);
  });

  it('draws only a well-formed card', () => {
    expect(
      isNotice({ title: 'UP-TAKE', body: 'A watch area changed.', serial: 1 }),
    ).toBe(true);
    expect(isNotice({ title: 'UP-TAKE', body: 'x' })).toBe(false);
    expect(isNotice({ title: 1, body: 'x', serial: 1 })).toBe(false);
    expect(isNotice(null)).toBe(false);
    expect(isNotice('card')).toBe(false);
  });
});
