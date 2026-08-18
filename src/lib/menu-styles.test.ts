import { describe, expect, it } from 'vitest';

import pageSvelte from '../routes/+page.svelte?raw';

/**
 * The menu's rendering, in the one file nothing else tests.
 *
 * Two rounds of independent review put two separate defects here, and neither
 * was reachable from Rust:
 *
 * Round 2, `F3`: `.menu-item.hovered` and `.menu-item.open` had equal
 * specificity and the open marker was declared second, so a parent row went
 * DIMMER at the moment the pointer rested on it -- which is the ordinary state,
 * since resting there is what opens the list. Fixed with `:not(.hovered)`.
 *
 * Round 3, `F2`: the markup that renders the child list was unguarded
 * altogether. `{#if menuFrame.child}` could be disabled, and the submenu never
 * rendered, with all 71 frontend tests and all 316 Rust tests green. Removing
 * `class:open={item.open}` silently disconnected the entire round-2 fix chain --
 * the Rust `owner` field, `menu_payload`, and the `:not(.hovered)` rule below
 * all exist to feed that one attribute.
 *
 * ⚠️ **The first version of this file opened with "Nothing tested
 * `+page.svelte` at all" and then read only the `<style>` block** of the file it
 * had already imported whole. That sentence was true when written and the file
 * did not act on it.
 *
 * ## Why this is a text test and not a rendered one
 *
 * There is no DOM in this suite: no jsdom, no component testing library, and
 * adding either is two devDependencies on a public GPL-3.0 binary. This reads
 * the source that already exists, in the suite that already runs, which is the
 * idiom `area-kinds.test.ts` established for the same reason.
 *
 * A text test pins shape rather than behaviour: it catches deletion and
 * neutering, and would not catch a rewrite that kept the same tokens while
 * changing what they do. That is weaker than a mutation drill against real
 * code, and it is said here rather than left to be discovered.
 *
 * ## What it CANNOT see, corrected
 *
 * A background arriving from an inline style, or from a rule outside the
 * `<style>` block. Anything about layout, geometry or actual rendering.
 *
 * ⚠️ **This list used to include "the two alphas being swapped so the guard is
 * satisfied while the row still dims", and that was false** -- the third test
 * below compares them and goes red in both directions. Round 3 `F5`: a stated
 * limitation that is not real invites someone to add a check that already
 * exists, or to distrust one that works.
 */

/** Everything inside the component's `<style>` block. */
function styleBlock(): string {
  const opens = pageSvelte.indexOf('<style>');
  const closes = pageSvelte.indexOf('</style>');
  if (opens < 0 || closes < 0) {
    throw new Error(
      'no <style> block in +page.svelte -- has the component been restructured?',
    );
  }
  return pageSvelte.slice(opens + '<style>'.length, closes);
}

/** Every rule in the style block, as `{selectors, body}`. */
function rules(): { selectors: string[]; body: string }[] {
  // Comments first: a selector inside `/* ... */` is not a selector, and the
  // file uses block comments to explain exactly these rules.
  const css = styleBlock().replace(/\/\*[\s\S]*?\*\//g, '');
  const found: { selectors: string[]; body: string }[] = [];
  for (const match of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    found.push({
      selectors: match[1]
        .split(',')
        .map((one) => one.trim())
        .filter(Boolean),
      body: match[2],
    });
  }
  if (found.length === 0) {
    throw new Error(
      'no CSS rules parsed out of +page.svelte -- an empty set agrees with anything',
    );
  }
  return found;
}

/** The body of the one rule carrying `selector` in its selector list. */
function rule(selector: string): string {
  const matching = rules().filter((one) => one.selectors.includes(selector));
  if (matching.length !== 1) {
    throw new Error(
      `expected exactly one rule for \`${selector}\`, found ${matching.length} -- renamed, or split across rules?`,
    );
  }
  return matching[0].body;
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

/** The markup of the child-list block, `{#if menuFrame.child}` to its `{/if}`. */
function childListMarkup(): string {
  const opens = pageSvelte.indexOf('{#if menuFrame.child}');
  if (opens < 0) {
    throw new Error(
      'no `{#if menuFrame.child}` block in +page.svelte -- the child list is not rendered, which is the whole of roadmap 1.28',
    );
  }
  const closes = pageSvelte.indexOf('{/if}', opens);
  if (closes < 0) {
    throw new Error('the child-list block is never closed');
  }
  return pageSvelte.slice(opens, closes);
}

const GUARDED = '.menu-item.open:not(.hovered)';

describe('the open parent row keeps its hover highlight', () => {
  it('guards the open marker so a hovered row cannot take it', () => {
    // Without `:not(.hovered)` this rule wins the cascade on a row that is
    // both, because it is declared second at equal specificity.
    expect(() => rule(GUARDED)).not.toThrow();
  });

  it('leaves an unguarded .menu-item.open in no rule at all', () => {
    // ⚠️ Round 3 `F3`: this compared whole trimmed LINES against one exact
    // spelling, so a selector LIST reintroduced the defect untouched --
    // `.menu-item.open,\n.menu-item.dummy {` declared later wins on a row
    // that is both, and the suite stayed at 71/71. Selector lists are split
    // now, so the shape of the declaration cannot hide it.
    const unguarded = rules()
      .filter((one) => one.selectors.includes('.menu-item.open'))
      .map((one) => one.selectors.join(', '));
    expect(
      unguarded,
      'an unguarded `.menu-item.open` selector is back; a row that is hovered AND open takes it',
    ).toEqual([]);
  });

  it('keeps the open marker dimmer than the hover it yields to', () => {
    // If the two were equal the ordering would not matter and neither would
    // this test; if `open` were the brighter the guard would hide the wrong
    // one. Goes red in both directions -- see the docstring correction.
    const hovered = backgroundAlpha(
      rule('.menu-item.hovered'),
      '.menu-item.hovered',
    );
    const open = backgroundAlpha(rule(GUARDED), GUARDED);
    expect(open).toBeLessThan(hovered);
  });
});

describe('the markup that renders the child list', () => {
  it('renders the child list at all', () => {
    // Round 3 `F2`, and the sharpest of them: `{#if false && menuFrame.child}`
    // left every gate green while the submenu never appeared. The pass
    // condition of roadmap 1.28 is this one line.
    expect(() => childListMarkup()).not.toThrow();
    expect(
      pageSvelte,
      'the child-list block is guarded by something other than `menuFrame.child` alone',
    ).toContain('{#if menuFrame.child}');
  });

  it('draws the parent row as open, which is what the whole Rust chain feeds', () => {
    // `owner` on the wire, `menu_payload`, `ChildMenuView.owner` and the
    // `:not(.hovered)` rule above all exist to set this one attribute. With
    // it removed they compute correctly and change nothing anyone can see.
    expect(pageSvelte).toContain('class:open={item.open}');
  });

  it('marks a row that opens a list', () => {
    // The affordance that says a row has children. `item.parent` is derived
    // in Rust from the row's own children, so a row cannot advertise a list
    // it does not have -- but only if this renders.
    expect(pageSvelte).toContain('{item.parent ?');
    expect(pageSvelte, 'the arrow glyph is gone').toContain('▸');
  });

  it('highlights the hovered row in BOTH lists', () => {
    // Two occurrences, and asserting the count is the point: the child list
    // having its own hover is a separate fact from the top level having one,
    // and round 3 removed it from the child rows alone.
    const all = pageSvelte.match(/class:hovered=\{item\.hovered\}/g) ?? [];
    expect(all.length, 'a list lost its hover highlight').toBe(2);
    expect(
      childListMarkup(),
      'the child rows no longer highlight what the pointer is on',
    ).toContain('class:hovered={item.hovered}');
  });
});
