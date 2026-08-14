import { describe, expect, it } from 'vitest';

import overlayRs from '../../src-tauri/src/overlay.rs?raw';
import overlayStateTs from './overlay-state.ts?raw';

/**
 * UP-TAKE `I-55`: the wire vocabulary is written out twice and only one copy
 * goes red.
 *
 * `AreaKind` in `overlay-state.ts` mirrors `type_name` in `overlay.rs` by hand,
 * and `ArmableType` mirrors `armable_type` the same way. Adding an `AreaType`
 * forces an arm in every exhaustive `match` on the Rust side, so that half
 * cannot be forgotten. Nothing at all happens on this side: the new name arrives
 * at runtime outside the union, with no type error and no failing test, and the
 * area draws as a default one. Roadmap 1.27 is the row that would have tripped
 * over it, because converting an area is the first thing that changes a `kind`
 * after creation.
 *
 * ## Why the Rust source and not a generated file
 *
 * A generator is the better answer and building one is not this test's job: it
 * needs a build step, a checked-in artefact and a CI check that the artefact is
 * current, which is three moving parts to keep a list of seven strings honest.
 * This reads the source that already exists, in the suite that already runs.
 *
 * ## What it cannot catch, and why every extraction throws
 *
 * A rename of a Rust function, or a reshaped `match`, makes the patterns below
 * find nothing. A silent empty set would compare equal to a union nobody had
 * updated either, which is this test passing for the exact reason it exists. So
 * each step throws instead, and both failure modes were drilled before this
 * shipped: an eighth type added to `type_name` fails the first case, and
 * renaming `type_name` fails it with the "has it been renamed?" message.
 */

/** The body of a Rust `fn <name>`, up to the first line that closes at column 0. */
function rustFunctionBody(name: string): string {
  const start = overlayRs.indexOf(`fn ${name}(`);
  if (start === -1) {
    throw new Error(`could not find Rust fn ${name}: has it been renamed?`);
  }
  const end = overlayRs.indexOf('\n}\n', start);
  if (end === -1) {
    throw new Error(`could not find the end of Rust fn ${name}`);
  }
  return overlayRs.slice(start, end);
}

/**
 * Every wire name a `match` in `body` pairs with an `AreaType` variant.
 *
 * Both directions, because the two functions face opposite ways: `type_name`
 * maps a variant to a string, `armable_type` maps a string to a variant.
 */
function wireNamesIn(name: string): string[] {
  const body = rustFunctionBody(name);
  const outbound = [
    ...body.matchAll(/AreaType::(\w+)\s*=>\s*(?:Some\()?"([a-z]+)"/g),
  ].map((match) => match[2]);
  const inbound = [
    ...body.matchAll(/"([a-z]+)"\s*=>\s*Some\(AreaType::(\w+)\)/g),
  ].map((match) => match[1]);
  const all = [...outbound, ...inbound];
  if (all.length === 0) {
    throw new Error(
      `extracted no names from Rust fn ${name}: has the match been reshaped?`,
    );
  }
  return all;
}

/** Every string literal in a TypeScript type alias, by the alias's name. */
function literalsIn(name: string): string[] {
  const declaration = new RegExp(`export type ${name}\\s*=([^;]*);`).exec(
    overlayStateTs,
  );
  if (declaration === null) {
    throw new Error(
      `could not find the ${name} declaration: has it been renamed?`,
    );
  }
  const members = [...declaration[1].matchAll(/'([a-z]+)'/g)].map(
    (match) => match[1],
  );
  if (members.length === 0) {
    throw new Error(
      `extracted no members from ${name}: has it stopped naming its literals?`,
    );
  }
  return members;
}

const sorted = (values: string[]) => [...values].sort();

describe('the wire vocabulary agrees across the IPC boundary', () => {
  it('AreaKind lists exactly the names type_name can send', () => {
    expect(sorted(literalsIn('AreaKind'))).toEqual(
      sorted(wireNamesIn('type_name')),
    );
  });

  it('ArmableType lists exactly the names armable_type accepts', () => {
    expect(sorted(literalsIn('ArmableType'))).toEqual(
      sorted(wireNamesIn('armable_type')),
    );
  });

  it('every armable name is also an AreaKind', () => {
    const kinds = literalsIn('AreaKind');
    for (const name of wireNamesIn('armable_type')) {
      expect(kinds).toContain(name);
    }
  });

  it('neither list repeats a name', () => {
    for (const name of ['AreaKind', 'ArmableType']) {
      const members = literalsIn(name);
      expect(sorted(members)).toEqual(sorted([...new Set(members)]));
    }
  });
});
