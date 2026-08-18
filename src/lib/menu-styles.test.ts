import { describe, expect, it } from 'vitest';

import pageSvelte from '../routes/+page.svelte?raw';

/**
 * UP-TAKE roadmap 1.28, round 2 of the independent review, finding `F3`: the
 * parent row of an open child list went DIMMER at the moment the pointer rested
 * on it.
 *
 * `.menu-item.hovered` and `.menu-item.open` have equal specificity, so the
 * later declaration wins. A parent row carries both classes for as long as the
 * pointer sits on it -- and sitting on it is what opens the list in the first
 * place -- so the highlight fell from the hover's 0.22 to the open marker's
 * 0.12 exactly when the row was being pointed at. The two backgrounds are
 * deliberately different; the ordering between them was not.
 *
 * ## Why this is a text test and not a rendered one
 *
 * There is no DOM in this suite: no jsdom, no component testing library, and
 * adding either is two devDependencies on a public GPL-3.0 binary for one
 * assertion. This reads the source that already exists, in the suite that
 * already runs, which is the idiom `area-kinds.test.ts` established for the
 * same reason.
 *
 * What it therefore CANNOT see: any other rule that sets a `.menu-item`
 * background, a background arriving from an inline style, or the two alphas
 * being swapped so the guard is satisfied while the row still dims. It pins the
 * one mechanism that failed, and says so rather than implying more.
 *
 * ## Why it matches lines rather than building a regex from the selector
 *
 * A selector is almost entirely regex metacharacters, so a pattern built from
 * one needs an escaping step, and an escaping step that quietly does nothing
 * leaves a pattern that matches nothing -- which is a test passing for the exact
 * reason it exists. Comparing trimmed lines has no escaping step to get wrong.
 * Every lookup throws when it finds nothing, for the same reason.
 */

/** The body of the one rule written as `<selector> {`, or a throw saying so. */
function rule(selector: string): string {
  const lines = pageSvelte.split('\n');
  const opens = lines.findIndex((line) => line.trim() === `${selector} {`);
  if (opens < 0) {
    throw new Error(
      `no rule written as \`${selector} {\` in +page.svelte -- renamed, or reformatted onto another line?`,
    );
  }
  const body: string[] = [];
  for (let at = opens + 1; at < lines.length; at += 1) {
    if (lines[at].trim() === '}') {
      return body.join('\n');
    }
    body.push(lines[at]);
  }
  throw new Error(`the rule for \`${selector}\` is never closed`);
}

/** The alpha of an `rgba(...)` background inside a rule body. */
function backgroundAlpha(body: string, selector: string): number {
  const found = body.match(/background:\s*rgba\([^)]*?,\s*([\d.]+)\s*\)/);
  if (!found) {
    throw new Error(
      `\`${selector}\` sets no rgba background -- has the highlight moved to another property?`,
    );
  }
  return Number(found[1]);
}

const GUARDED = '.menu-item.open:not(.hovered)';

describe('the open parent row keeps its hover highlight', () => {
  it('guards the open marker so a hovered row cannot take it', () => {
    // The whole fix. Without `:not(.hovered)` this rule wins the cascade on a
    // row that is both, because it is declared second at equal specificity.
    expect(() => rule(GUARDED)).not.toThrow();
  });

  it('leaves an unguarded .menu-item.open nowhere in the file', () => {
    // A reintroduced copy would win over the guarded one by being later, so
    // the guard existing somewhere is not on its own the property wanted.
    const unguarded = pageSvelte
      .split('\n')
      .filter((line) => line.trim() === '.menu-item.open {');
    expect(
      unguarded,
      'an unguarded `.menu-item.open` rule is back; a row that is hovered AND open takes it',
    ).toEqual([]);
  });

  it('keeps the open marker dimmer than the hover it yields to', () => {
    // The reason the guard is needed at all. If the two were equal the
    // ordering would not matter and neither would this test; if `open` were
    // the brighter of the two the guard would be hiding the wrong one.
    const hovered = backgroundAlpha(
      rule('.menu-item.hovered'),
      '.menu-item.hovered',
    );
    const open = backgroundAlpha(rule(GUARDED), GUARDED);
    expect(open).toBeLessThan(hovered);
  });
});
