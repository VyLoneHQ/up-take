import { describe, expect, it } from 'vitest';

import hotkeyRs from '../../src-tauri/src/hotkey.rs?raw';
import { BUTTONS, DRAW, MODES, progress, REFERENCE, TYPES } from './coach-copy';
import { armedTypeForKey, isFreezeKey, isRemoveKey } from './overlay-state';
import { isDismissKey } from './regions';

/**
 * The reference sheet against the code that binds its keys (roadmap 1.18).
 *
 * ADR-0043 decision 5 makes step 4 the single source of the keybind reference.
 * A single source is only worth having if it is right, and the ways it goes
 * wrong are all silent: a shortcut renamed in `hotkey.rs`, a type letter moved
 * in `armedTypeForKey`, a key dropped from a handler. Each test below reads the
 * binding rather than restating it.
 */

/** The string value of a `pub const NAME: &str = "..."` in `hotkey.rs`. */
function rustLabel(name: string): string {
  const found = hotkeyRs.match(
    new RegExp(`pub const ${name}: &str = "([^"]+)";`),
  );
  if (!found) {
    throw new Error(
      `no \`pub const ${name}: &str\` in hotkey.rs. Renamed? An unfound label must not read as a match.`,
    );
  }
  return found[1];
}

/** A keydown with no modifiers, for the handlers that take one. */
function press(key: string) {
  return { key, ctrlKey: false, altKey: false, metaKey: false };
}

describe('the reference sheet names the keys the app actually binds', () => {
  it('names the summon shortcut hotkey.rs registers', () => {
    const summon = rustLabel('SUMMON_LABEL');
    expect(REFERENCE.anywhere.map((row) => row.keys)).toContain(summon);
    expect(MODES.chord.join('+')).toBe(summon);
  });

  it('names the grab shortcut hotkey.rs registers', () => {
    expect(REFERENCE.anywhere.map((row) => row.keys)).toContain(
      rustLabel('GRAB_LABEL'),
    );
  });

  it('gives every type row a key that arms that type', () => {
    const armed = TYPES.rows.map((row) =>
      armedTypeForKey(press(row.key.toLowerCase())),
    );
    expect(armed).toEqual(['screenshot', 'filter', 'upscale', 'ocr']);
  });

  it('lists every letter that arms a type, and no other', () => {
    // Swept over the alphabet rather than read from the rows, so a type that
    // gains a key in `armedTypeForKey` and not here fails.
    // Compared as sorted sets: the rows keep the mockup's teaching order, which
    // is not the alphabet's, and the order is not what this pins.
    const arming = [...'abcdefghijklmnopqrstuvwxyz']
      .filter((letter) => armedTypeForKey(press(letter)) !== null)
      .map((letter) => letter.toUpperCase());
    const sorted = (keys: string[]) => [...keys].sort();
    expect(sorted(TYPES.rows.map((row) => row.key))).toEqual(arming);
    const sheet = REFERENCE.placing.find((row) => row.does.includes('type'));
    expect(sorted(sheet?.keys.split(' ') ?? [])).toEqual(arming);
  });

  it('names the freeze, remove and leave keys the handlers act on', () => {
    const keys = REFERENCE.placing.map((row) => row.keys);
    expect(keys).toContain('Ctrl+Space');
    expect(
      isFreezeKey({ key: ' ', ctrlKey: true, altKey: false, metaKey: false }),
    ).toBe(true);
    expect(keys).toContain('Delete');
    expect(isRemoveKey('Delete')).toBe(true);
    // `Esc` is the printed name; `Escape` is the DOM's. The mapping between
    // them is the whole of what this pins.
    expect(keys).toContain(DRAW.escKey);
    expect(isDismissKey('Escape')).toBe(true);
  });
});

describe('the coach copy is public writing', () => {
  it('carries no em dash or en dash (P-1)', () => {
    const strings = JSON.stringify({ BUTTONS, DRAW, TYPES, MODES, REFERENCE });
    // Escaped rather than typed: the literal characters would put two dashes
    // into this file, which is the thing the test exists to keep out.
    expect(strings).not.toMatch(/[\u2013\u2014]/);
  });

  it('counts steps the way the coach prints them', () => {
    expect(progress(2, 4)).toBe('Step 2 of 4');
  });
});
