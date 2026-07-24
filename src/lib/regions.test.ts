import { describe, expect, it } from 'vitest';

import { isDismissKey } from './regions';

// The `measure`/`reportInteractiveRegions` suites that lived here were deleted
// with the functions themselves (ADR-0016: the overlay window is never
// interactive, so the frontend no longer reports regions for Rust to hit-test).

describe('isDismissKey', () => {
  it('accepts Escape', () => {
    expect(isDismissKey('Escape')).toBe(true);
  });

  it('rejects everything else', () => {
    for (const key of ['Esc', 'escape', 'Enter', ' ', 'a']) {
      expect(isDismissKey(key)).toBe(false);
    }
  });
});
