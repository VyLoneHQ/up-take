import { describe, expect, it } from 'vitest';

import changelogMd from '../../CHANGELOG.md?raw';
import contributingMd from '../../CONTRIBUTING.md?raw';
import readmeMd from '../../README.md?raw';
import placementRs from '../../src-tauri/src/placement.rs?raw';

/**
 * UP-TAKE `I-291`: every public page states how many area types are built, and
 * nothing checks any of them against the code.
 *
 * ## Why this exists, which is a measurement rather than a worry
 *
 * On 2026-08-22 a single session shipped four enumerations of one class and
 * every one came back short, each written by its own author:
 *
 * - a reviewer's list claimed 11 members; the true count was 15.
 * - the commit that fixed those claimed 15 and *named 14*; a repo-wide re-sweep
 *   found 17.
 * - a backlog row said one false sentence had four copies; it had six.
 * - and the fix for the miss was itself wrong twice running.
 *
 * Three of the misses were **public**. `README.md` said "Two of seven area
 * types are built" for ten days after Filter shipped. `CONTRIBUTING.md` said
 * the same thing, and was missed by the change that corrected the README **in
 * the commit that wrote "update it in the same change that ships a type" into
 * the README**. `CHANGELOG.md` said "Three area types" and enumerated three.
 *
 * The lesson is not "write better lists". Two independent review rounds caught
 * these by reading, which is exactly the effort a gate exists to stop spending.
 * So the count is **derived**, once, from the code, and every page is compared
 * against it.
 *
 * ## Why `conversion_label` is the source of truth
 *
 * "Built" means *has behaviour a user can reach*, and `conversion_label` is
 * where that is already decided: it returns `Some` for the types the area menu
 * offers and `None` for the ones that are modelled and do nothing. It is not a
 * second list invented for this test. A type gaining behaviour has to add an
 * arm there or it does not appear in the menu, so the number cannot drift away
 * from the product without the menu drifting too.
 *
 * `AreaType::ALL` would be the wrong source: it is all seven, built or not.
 *
 * ## What it cannot catch
 *
 * It checks the NUMBER, not the prose around it. A page that says "four area
 * types" and then lists the wrong four still passes here; `CHANGELOG.md`'s
 * enumeration is prose and only a reader can check it. It also only knows the
 * three pages named above, so a fourth public page stating a count is invisible
 * until someone adds it to `PAGES` -- an obligation, and the honest form of
 * this doc names it rather than implying the sweep is complete.
 *
 * Every extraction throws rather than returning nothing, for the reason
 * `area-kinds.test.ts` gives at length: a silent empty result agrees with any
 * number at all, which is this control passing for the exact reason it exists.
 */

/** Number words as the pages spell them, lowest first. */
const NUMBER_WORDS = [
  'zero',
  'one',
  'two',
  'three',
  'four',
  'five',
  'six',
  'seven',
] as const;

/** The public pages that state a built-type count, and how each phrases it. */
const PAGES = [
  {
    name: 'README.md',
    source: readmeMd,
    // "Four of seven area types are built."
    pattern: /\b(\w+) of seven area types are built\b/i,
  },
  {
    name: 'CONTRIBUTING.md',
    source: contributingMd,
    // "Four of seven area types are built and there is no release yet."
    pattern: /\b(\w+) of seven area types are built\b/i,
  },
  {
    name: 'CHANGELOG.md',
    source: changelogMd,
    // "- Four area types, chosen from the area's own right-click menu."
    pattern: /\b(\w+) area types, chosen from\b/i,
  },
] as const;

/**
 * How many area types have behaviour, read out of `conversion_label`.
 *
 * Counts the arms answering `Some`, which is the menu's own definition of a
 * type that does something.
 */
function builtTypeCount(): number {
  const start = placementRs.indexOf('fn conversion_label(');
  if (start === -1) {
    throw new Error(
      'could not find `fn conversion_label(` in placement.rs: has it been renamed? ' +
        'An unfound function must not read as a count of zero.',
    );
  }
  const end = placementRs.indexOf('\n}\n', start);
  if (end === -1) {
    throw new Error('could not find the end of `conversion_label`');
  }
  const body = placementRs.slice(start, end);

  // `AreaType::Upscale => Some("Type: Upscale"),` -- one arm, one built type.
  // Comment lines are excluded because a doc comment inside the match may quote
  // an arm; `payload-keys.test.ts` shipped that exact false positive.
  const arms = body
    .split('\n')
    .filter((line) => !line.trimStart().startsWith('//'))
    .join('\n')
    .match(/AreaType::\w+\s*=>\s*Some\(/g);

  if (arms === null) {
    throw new Error(
      '`conversion_label` yielded no `Some` arms: has the match been reshaped? ' +
        'An empty extraction agrees with any number.',
    );
  }
  return arms.length;
}

describe('the built-area-type count', () => {
  it('is readable from conversion_label and is not absurd', () => {
    const count = builtTypeCount();
    // Bounds rather than a literal, so shipping a type does not fail this line
    // and tempt the next author to loosen the file. Seven is every type there
    // is; below two the extraction is more likely broken than the product.
    expect(count).toBeGreaterThanOrEqual(2);
    expect(count).toBeLessThanOrEqual(7);
  });

  it.each(PAGES)(
    '$name states the same count the code does',
    ({ name, source, pattern }) => {
      const match = source.match(pattern);
      if (match === null) {
        throw new Error(
          `${name} no longer contains a sentence matching ${pattern}. Either the ` +
            'count was removed, or it was reworded and this control silently ' +
            'stopped covering that page. Update the pattern in the same change.',
        );
      }

      const stated = NUMBER_WORDS.indexOf(
        match[1].toLowerCase() as (typeof NUMBER_WORDS)[number],
      );
      expect(
        stated,
        `${name} says "${match[1]}", which is not a number word this control knows`,
      ).toBeGreaterThanOrEqual(0);

      expect(
        stated,
        `${name} says ${match[1]} area types are built; conversion_label has ` +
          `${builtTypeCount()} arms returning Some. A public page is under- or ` +
          'over-reporting the product.',
      ).toBe(builtTypeCount());
    },
  );
});
