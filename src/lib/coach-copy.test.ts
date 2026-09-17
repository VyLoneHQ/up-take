import { describe, expect, it } from 'vitest';

import hotkeyRs from '../../src-tauri/src/hotkey.rs?raw';
import { coachCopy } from './coach-copy';
import {
  armedTypeForKey,
  isFreezeKey,
  isRemoveKey,
  kindLabels,
} from './overlay-state';
import { isDismissKey } from './regions';
import { LANGUAGES } from './strings';

/**
 * The reference sheet against the code that binds its keys (roadmap 1.18).
 *
 * ADR-0043 decision 5 makes step 4 the single source of the keybind reference.
 * A single source is only worth having if it is right, and the ways it goes
 * wrong are all silent: a shortcut renamed in `hotkey.rs`, a type letter moved
 * in `armedTypeForKey`, a key dropped from a handler. Each test below reads the
 * binding rather than restating it.
 *
 * The English copy is the one these read, because the tests that find a row by
 * its words need one language. `every language prints the same keys` below is
 * what carries the binding checks over to the translations.
 */
const { BUTTONS, DRAW, MODES, progress, REFERENCE, TYPES } = coachCopy('en');
const KIND_LABELS = kindLabels('en');

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

  it('every language prints the same keys and chords as English', () => {
    // The words translate and the keys do not, so every binding check above
    // holds for each translation too. `drag` is the one key word, and it is the
    // only cell allowed to differ.
    const shape = (language: (typeof LANGUAGES)[number]) => {
      const copy = coachCopy(language);
      const drag = copy.REFERENCE.placing[0].keys;
      return {
        chord: copy.MODES.chord,
        esc: copy.DRAW.escKey,
        types: copy.TYPES.rows.map((row) => row.key),
        anywhere: copy.REFERENCE.anywhere.map((row) =>
          row.keys.replace(drag, 'drag'),
        ),
        placing: copy.REFERENCE.placing.map((row) =>
          row.keys === drag ? 'drag' : row.keys,
        ),
      };
    };
    for (const language of LANGUAGES) {
      expect(shape(language), language).toEqual(shape('en'));
    }
  });
});

describe('the coach copy is public writing', () => {
  it('carries no em dash or en dash in English (P-1)', () => {
    const strings = JSON.stringify({ BUTTONS, DRAW, TYPES, MODES, REFERENCE });
    // Built from code points rather than typed: the literal characters would
    // put two dashes into this file, which is the thing the test exists to keep
    // out. (This was an escape sequence until 1.38, when a tool turned it into
    // the characters themselves.)
    expect(strings).not.toMatch(
      new RegExp(`[${String.fromCodePoint(0x2013, 0x2014)}]`),
    );
  });

  it('counts steps the way the coach prints them', () => {
    expect(progress(2, 4)).toBe('Step 2 of 4');
    expect(coachCopy('de').progress(2, 4)).toBe('Schritt 2 von 4');
  });
});

describe('the tour calls each type what the rest of the product calls it', () => {
  it('names every type row with the label the type bar and menu use, in every language', () => {
    // The independent review of 1.18: the tour said "Text" for the type the
    // menu and the bar call "OCR". The rows read `kindLabels` now; this pins
    // that, through the key, so a row whose key and name point at different
    // types fails as well as a row with a word of its own.
    for (const language of LANGUAGES) {
      const labels = kindLabels(language);
      for (const row of coachCopy(language).TYPES.rows) {
        const kind = armedTypeForKey(press(row.key.toLowerCase()));
        expect(kind, `${row.key} arms nothing`).not.toBeNull();
        if (kind === null) continue;
        expect(row.name, `the ${row.key} row (${language})`).toBe(labels[kind]);
      }
    }
    expect(KIND_LABELS.ocr).toBe('OCR');
  });
});
