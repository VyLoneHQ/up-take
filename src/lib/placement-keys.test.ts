/**
 * The keys Rust takes on the page's behalf must be the keys the page acts on.
 *
 * Since `up-take` `#112`, a Placement key pressed while another program has the
 * keyboard never reaches this page: the keyboard hook in `placement.rs` takes
 * it and runs the same command the page would have. So the arming letters exist
 * twice, here in `armedTypeForKey` and there in `ARM_KEYS`, and a letter added
 * to one and not the other would arm from one route and not the other. This
 * reads the Rust table and holds the two to each other, in both directions.
 */
import { describe, expect, test } from 'vitest';
import placementRs from '../../src-tauri/src/placement.rs?raw';
import { armedTypeForKey } from './overlay-state';

/** `ARM_KEYS` as written in `placement.rs`: upper-case letter to type name. */
function rustArmKeys(): Map<string, string> {
  const start = placementRs.indexOf('const ARM_KEYS:');
  expect(start, 'ARM_KEYS is gone from placement.rs').toBeGreaterThan(-1);
  const end = placementRs.indexOf('];', start);
  const block = placementRs.slice(start, end);
  const entries = [...block.matchAll(/\(b'([A-Z])', "([a-z]+)"\)/g)];
  const declared = /\[\(u8, &str\); (\d+)\]/.exec(block);
  // Every entry parsed, or the comparison below is over a partial table.
  expect(entries.length, 'an ARM_KEYS entry did not parse').toBe(
    Number(declared?.[1]),
  );
  return new Map(entries.map((match) => [match[1], match[2]]));
}

const bare = (key: string) => ({
  key,
  ctrlKey: false,
  altKey: false,
  metaKey: false,
});

describe('the keyboard hook arms exactly what the page arms', () => {
  test('every letter maps to the same type on both routes', () => {
    const rust = rustArmKeys();
    for (const letter of 'ABCDEFGHIJKLMNOPQRSTUVWXYZ') {
      expect(
        armedTypeForKey(bare(letter.toLowerCase())),
        `letter ${letter}`,
      ).toBe(rust.get(letter) ?? null);
    }
  });

  test('the table is not empty, so the loop above compared something', () => {
    expect(rustArmKeys().size).toBeGreaterThan(0);
  });
});
