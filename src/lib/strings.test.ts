import { describe, expect, it } from 'vitest';

import catalogue from '../../locales/strings.json';
import stringsRs from '../../src-tauri/src/strings.rs?raw';
import { fill, fingerprint, isLanguage, LANGUAGES, text } from './strings';

/**
 * The catalogue's health (roadmap 1.38).
 *
 * The drift that matters in a translated product is not a missing key, which
 * shows up at once, but a translation that is still present after its English
 * changed and now says something else. So each translation records a
 * fingerprint of the English it was made from, and the stale-translation test
 * below fails until the translation is updated or re-confirmed.
 */

type Entry = Record<string, string>;
const entries = Object.entries(catalogue.strings) as [string, Entry][];
const translations = LANGUAGES.filter((language) => language !== 'en');

/**
 * An en-dash or an em-dash, built from code points: typed literally, the
 * characters this looks for would be in this file.
 */
const DASHES = new RegExp(`[${String.fromCodePoint(0x2013, 0x2014)}]`);

/** The `{name}` placeholders in a string, sorted. */
function placeholders(source: string): string[] {
  return [...source.matchAll(/\{(\w+)\}/g)].map((match) => match[1]).sort();
}

/**
 * Every source file that may use a key. Tests are excluded, so a key used only
 * by its own test still counts as unused.
 */
const SOURCES = import.meta.glob(
  ['../**/*.{ts,svelte}', '!../**/*.test.ts', '!./strings.ts'],
  { query: '?raw', import: 'default', eager: true },
) as Record<string, string>;

/** The keys `strings.rs` names, read out of its `texts!` block. */
function rustKeys(): string[] {
  const found = [...stringsRs.matchAll(/^\s*\w+ => "([a-z0-9_.]+)",$/gm)].map(
    (match) => match[1],
  );
  if (found.length === 0) {
    throw new Error(
      'no `Variant => "key",` lines in strings.rs: has the texts! block been reshaped? An empty list agrees with any catalogue.',
    );
  }
  return found;
}

describe('the fingerprint', () => {
  it('is FNV-1a 32, by its published test vectors', () => {
    expect(fingerprint('')).toBe('811c9dc5');
    expect(fingerprint('a')).toBe('e40c292c');
    expect(fingerprint('foobar')).toBe('bf9cf968');
  });

  it('hashes UTF-8 bytes rather than UTF-16 units', () => {
    // Built from code points, so no editor or formatter can turn them into
    // something else. U+00E4 is two bytes in UTF-8 (C3 A4); an implementation
    // hashing UTF-16 units would hash the single byte E4 and get 610b5af3.
    const umlaut = String.fromCodePoint(0xe4);
    expect(fingerprint(umlaut)).toBe('199de0e2');
    expect(fingerprint(`Gr${String.fromCodePoint(0xf6, 0xdf)}e`)).toBe(
      '48b3427a',
    );
  });
});

describe('the catalogue', () => {
  it('lists English first, once, because every lookup falls back to it', () => {
    // The language list is the catalogue's alone (roadmap 1.38: a third
    // language is a change to that file), so this pins only what the code
    // relies on: English leads, and no code appears twice.
    expect(LANGUAGES[0]).toBe('en');
    expect(new Set(LANGUAGES).size).toBe(LANGUAGES.length);
  });

  it('has every language for every string, and no field it does not expect', () => {
    const allowed = new Set([
      'en',
      ...translations.flatMap((language) => [language, `${language}_from`]),
    ]);
    for (const [key, entry] of entries) {
      for (const language of LANGUAGES) {
        expect(entry[language], `${key} has no ${language} text`).toBeTruthy();
      }
      for (const field of Object.keys(entry)) {
        expect(
          allowed.has(field),
          `${key} has an unexpected field ${field}`,
        ).toBe(true);
      }
    }
  });

  it('has no translation older than its English', () => {
    const stale: string[] = [];
    for (const [key, entry] of entries) {
      const current = fingerprint(entry.en);
      for (const language of translations) {
        if (entry[`${language}_from`] !== current) {
          stale.push(
            `${key} (${language}): the English is now "${entry.en}". Update the ${language} text if it no longer fits, then set ${language}_from to "${current}".`,
          );
        }
      }
    }
    expect(stale, stale.join('\n')).toEqual([]);
  });

  it('uses the same placeholders in every language', () => {
    for (const [key, entry] of entries) {
      for (const language of translations) {
        expect(placeholders(entry[language]), `${key} (${language})`).toEqual(
          placeholders(entry.en),
        );
      }
    }
  });

  it('writes no em-dash or en-dash in English', () => {
    // P-1 for public writing. German is exempt: the Gedankenstrich is correct
    // German punctuation (VOICE.md section 7).
    for (const [key, entry] of entries) {
      expect(DASHES.test(entry.en), key).toBe(false);
    }
  });

  it('holds every key Rust names', () => {
    for (const key of rustKeys()) {
      expect(key in catalogue.strings, `${key} is named in strings.rs`).toBe(
        true,
      );
    }
  });

  it('holds no key that nothing uses', () => {
    const rust = new Set(rustKeys());
    const page = Object.values(SOURCES).join('\n');
    const unused = entries
      .map(([key]) => key)
      .filter((key) => !rust.has(key) && !page.includes(`'${key}'`));
    expect(unused).toEqual([]);
  });
});

describe('lookup', () => {
  it('reads each language and fills placeholders once', () => {
    expect(text('en', 'coach.next')).toBe('Next');
    expect(text('de', 'coach.next')).toBe('Weiter');
    expect(fill('en', 'coach.progress', { step: 2, total: 4 })).toBe(
      'Step 2 of 4',
    );
    expect(fill('de', 'coach.progress', { step: 2, total: 4 })).toBe(
      'Schritt 2 von 4',
    );
    expect(fill('en', 'coach.progress', { step: '{total}', total: 4 })).toBe(
      'Step {total} of 4',
    );
    expect(fill('en', 'coach.progress', {})).toBe('Step {step} of {total}');
  });

  it('accepts only the languages it ships', () => {
    expect(isLanguage('de')).toBe(true);
    expect(isLanguage('en')).toBe(true);
    expect(isLanguage('fr')).toBe(false);
    expect(isLanguage(undefined)).toBe(false);
  });
});
