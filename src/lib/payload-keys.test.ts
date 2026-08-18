import { describe, expect, it } from 'vitest';

import overlayRs from '../../src-tauri/src/overlay.rs?raw';
import placementRs from '../../src-tauri/src/placement.rs?raw';
import overlayStateTs from './overlay-state.ts?raw';

/**
 * UP-TAKE `I-67`, round 2 finding `F7`: `overlay-state.ts` is a second
 * hand-written copy of all twelve payload shapes and nothing compared it to
 * Rust.
 *
 * `src-tauri/src/payload_keys.rs` closed one half of this. It pins what each
 * payload actually serializes to, by serializing it -- which is the only thing
 * that can see a `#[serde(rename_all)]`, and the reason `UT-F-72` existed. But
 * it pins the RUST side alone. The frontend indexes these payloads through
 * interfaces typed out by hand on this side, and a field renamed in Rust with
 * its key table updated in the same commit leaves this file describing a wire
 * that no longer exists: `svelte-check` is green, because a TypeScript
 * interface is a claim about incoming JSON that nothing verifies, and the read
 * is `undefined` at runtime.
 *
 * The backlog row's own words are that "the repo already pays this cost twice",
 * and the `ts-rs`/`specta` rejection never engaged with that. This is the third
 * copy noticing the second.
 *
 * ## Why the key literals in the Rust tests are a trustworthy source
 *
 * They would not be, on their own -- they are string literals somebody typed.
 * What makes them usable is that `assert_keys` compares each one against
 * `serde_json` output for a real value of that type, in a test that runs in CI.
 * So the literal list is not a claim ABOUT the wire, it is a value already
 * checked against the wire, and reading it here needs no build step, no
 * generated artefact and no CI check that the artefact is current -- the three
 * moving parts `payload_keys.rs` gives as its reason for not generating.
 *
 * ## What this CANNOT see
 *
 * Types, entirely: `hovered: number | null` against a Rust `Option<usize>` is
 * not checked here and is not checkable from text. Only the set of keys. A
 * payload that reaches the frontend without an `assert_keys` call is invisible
 * to this file, which is what `payload_keys.rs`'s own coverage control exists
 * to make impossible. And an interface this file does not name is unchecked --
 * so the pairing is asserted to be complete, below, rather than left to whoever
 * adds the next payload.
 */

/** Rust payload type name -> the interface name this file gives it. */
const RENAMED: Record<string, string> = {
  // The one pair whose names differ. Rust emits one area as `AreaPayload`;
  // this side calls the same object an `AreaView`, because on the frontend it
  // is what gets drawn rather than what arrived. Documented rather than
  // renamed: renaming either side is a change to a public-ish surface for
  // tidiness, and the point of this file is that the two are checked, not that
  // they are spelled alike.
  AreaPayload: 'AreaView',
};

/** The arguments of a call, split at top level, given the text inside its parens. */
function argumentsOf(inside: string): string[] {
  const args: string[] = [];
  let depth = 0;
  let quoted = false;
  let start = 0;
  for (let at = 0; at < inside.length; at += 1) {
    const ch = inside[at];
    if (quoted) {
      if (ch === '"' && inside[at - 1] !== String.fromCharCode(92))
        quoted = false;
      continue;
    }
    if (ch === '"') quoted = true;
    else if (ch === '(' || ch === '[' || ch === '{') depth += 1;
    else if (ch === ')' || ch === ']' || ch === '}') depth -= 1;
    else if (ch === ',' && depth === 0) {
      args.push(inside.slice(start, at).trim());
      start = at + 1;
    }
  }
  const last = inside.slice(start).trim();
  if (last) args.push(last);
  return args;
}

/** The string literals in a `&["a", "b"]` slice literal. */
function literals(slice: string): string[] {
  return [...slice.matchAll(/"([^"]*)"/g)].map((m) => m[1]);
}

/**
 * The keys named by the last argument of an `assert_keys` call.
 *
 * That argument is a slice literal at most call sites and a named `const` at
 * one -- `StatePayload`'s list is long enough to have been lifted out. Reading
 * only the literal form silently produced no keys for that payload, and a
 * payload with no keys is a payload this file does not compare, which is this
 * test passing for the reason it exists. So an unresolvable argument throws.
 */
function keysOf(argument: string, source: string, what: string): string[] {
  if (argument.startsWith('&[')) {
    return literals(argument);
  }
  const named = argument.match(/^[A-Z][A-Z0-9_]*$/);
  if (named) {
    const declaration = source.match(
      new RegExp(`const ${argument}: &\\[&str\\] = &\\[([^\\]]*)\\]`),
    );
    if (declaration) {
      return literals(declaration[1]);
    }
  }
  throw new Error(
    `${what}: cannot read the key list from \`${argument}\` -- a slice literal or a \`const NAME: &[&str]\` in the same file are the two forms this understands.`,
  );
}

/** The `assert_keys` calls in one Rust source: type name -> sorted keys. */
function rustPayloadKeys(source: string, what: string): Map<string, string[]> {
  const found = new Map<string, string[]>();
  let at = source.indexOf('assert_keys(');
  while (at >= 0) {
    // Walk to the matching close paren so a multi-line call reads the same
    // as a single-line one.
    let depth = 0;
    let end = at + 'assert_keys'.length;
    for (; end < source.length; end += 1) {
      const ch = source[end];
      if (ch === '(') depth += 1;
      if (ch === ')') {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    const args = argumentsOf(source.slice(at + 'assert_keys('.length, end));
    const name = args[0]?.match(/^"([A-Za-z_][A-Za-z0-9_]*)"$/);
    if (name && args.length >= 2) {
      const keys = keysOf(args[args.length - 1], source, what);
      if (keys.length === 0) {
        throw new Error(
          `${what}: \`${name[1]}\` resolved to an empty key list.`,
        );
      }
      found.set(name[1], keys.slice().sort());
    }
    at = source.indexOf('assert_keys(', end);
  }
  if (found.size === 0) {
    throw new Error(
      `no assert_keys calls found in ${what} -- renamed, or reshaped? An empty set agrees with anything.`,
    );
  }
  return found;
}

/** The property names of every `export interface` in a TypeScript source. */
function tsInterfaceKeys(source: string): Map<string, string[]> {
  // Block comments first: JSDoc here contains `{@link ...}`, whose braces
  // would otherwise be counted as structure.
  const stripped = source
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/\/\/.*$/gm, '');
  const found = new Map<string, string[]>();
  const lines = stripped.split('\n');
  for (let i = 0; i < lines.length; i += 1) {
    const opens = lines[i].match(/^export interface ([A-Za-z0-9_]+)\s*\{/);
    if (!opens) continue;
    const keys: string[] = [];
    let depth = 1;
    for (let j = i + 1; j < lines.length && depth > 0; j += 1) {
      const line = lines[j];
      const before = depth;
      depth +=
        (line.match(/\{/g) ?? []).length - (line.match(/\}/g) ?? []).length;
      if (before !== 1) continue;
      const property = line.match(/^\s{2}([A-Za-z0-9_]+)\??\s*:/);
      if (property) keys.push(property[1]);
    }
    found.set(opens[1], keys.slice().sort());
  }
  if (found.size === 0) {
    throw new Error('no exported interfaces found in overlay-state.ts');
  }
  return found;
}

const rust = new Map([
  ...rustPayloadKeys(overlayRs, 'overlay.rs'),
  ...rustPayloadKeys(placementRs, 'placement.rs'),
]);
const ts = tsInterfaceKeys(overlayStateTs);

describe('every payload arrives under the keys this side reads', () => {
  it('finds a payload table on both sides', () => {
    // The silent failure this whole file is exposed to: two extractions that
    // find nothing agree perfectly.
    expect(rust.size).toBeGreaterThanOrEqual(12);
    expect(ts.size).toBeGreaterThanOrEqual(12);
  });

  it.each([...rust.keys()])('%s has the same keys in TypeScript', (name) => {
    const tsName = RENAMED[name] ?? name;
    const declared = ts.get(tsName);
    expect(
      declared,
      `Rust pins keys for \`${name}\` and \`overlay-state.ts\` declares no \`${tsName}\`. Add the interface, or map the name in RENAMED with a reason.`,
    ).toBeDefined();
    expect(declared).toEqual(rust.get(name));
  });

  it('leaves no Rust payload unpaired', () => {
    // `it.each` over an empty list passes, so the count is asserted apart
    // from the comparison it drives.
    const unpaired = [...rust.keys()].filter(
      (name) => !ts.has(RENAMED[name] ?? name),
    );
    expect(unpaired).toEqual([]);
  });
});
