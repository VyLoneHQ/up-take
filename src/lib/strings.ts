/**
 * Every word the page shows, in the language the user reads (roadmap 1.38).
 *
 * The words live in `locales/strings.json`, one entry per string with its
 * languages side by side, and the Rust side reads the same file through
 * `src-tauri/src/strings.rs`. Rust decides the language (Windows' display
 * language, and roadmap 1.14's setting once it exists) and the page asks for it
 * once when it mounts, so the menu Rust builds and the tour drawn here are
 * always in the same language.
 *
 * `strings.test.ts` checks the catalogue: every language present for every
 * string, the same placeholders in each, no key nobody uses, and a fingerprint
 * on each translation that goes stale when its English changes.
 */

import catalogue from '../../locales/strings.json';

/** A language code the catalogue carries, such as `en` or `pt-BR`. */
export type Language = string;

/**
 * Every language, in the catalogue's own order, English first because it is the
 * fallback. Read from the catalogue, so a language added there needs no change
 * here.
 */
export const LANGUAGES: readonly Language[] = catalogue.languages;

/** A key in the catalogue. Typed, so a misspelt key fails `svelte-check`. */
export type TextKey = keyof typeof catalogue.strings;

/** Whether a value from Rust names a language the catalogue carries. */
export function isLanguage(value: unknown): value is Language {
  return typeof value === 'string' && LANGUAGES.includes(value);
}

/** A string in a language, falling back to English. */
export function text(language: Language, key: TextKey): string {
  const entry: Record<string, string> = catalogue.strings[key];
  return entry[language] || entry.en;
}

/**
 * A string with its `{name}` placeholders filled, in one pass, so a value that
 * itself contains braces is never expanded again. A placeholder with no value
 * is left as written, where a reader will see it.
 */
export function fill(
  language: Language,
  key: TextKey,
  values: Readonly<Record<string, string | number>>,
): string {
  return text(language, key).replace(/\{(\w+)\}/g, (whole, name: string) =>
    Object.hasOwn(values, name) ? String(values[name]) : whole,
  );
}

/**
 * The fingerprint a translation records of the English it was made from: FNV-1a
 * over the UTF-8 bytes, as eight hex digits. Not a security measure. It only has
 * to change when the English does.
 */
export function fingerprint(source: string): string {
  let hash = 0x811c9dc5;
  for (const byte of new TextEncoder().encode(source)) {
    hash ^= byte;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}
