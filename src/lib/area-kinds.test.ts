import { describe, expect, it } from 'vitest';

import overlayRs from '../../src-tauri/src/overlay.rs?raw';
import overlayStateTs from './overlay-state.ts?raw';

/**
 * UP-TAKE `I-55`: the wire vocabulary is written out twice and only one copy
 * goes red.
 *
 * Four TypeScript string-literal unions in `overlay-state.ts` mirror four
 * exhaustive Rust `match`es in `overlay.rs` by hand. Adding a variant forces an
 * arm on the Rust side, so that half cannot be forgotten. Nothing at all happens
 * on this side: the new name arrives at runtime outside the union, with no type
 * error and no failing test, and whatever switches on it silently takes its
 * default branch. Roadmap 1.27 is the row that would have tripped over it,
 * because converting an area is the first thing that changes a `kind` after
 * creation.
 *
 * ## All four pairs, not only the one the feature touched
 *
 * `AreaKind`/`type_name` and `ArmableType`/`armable_type` are the pair 1.27
 * needed. `LayerName`/`layer_name` and `OverlayStateName`/`state_name` have the
 * identical defect and were found by the independent review of the change that
 * covered the first two: a class fixed at one member is a class not fixed.
 * `overlay.rs`'s own doc for `type_name` points at `layer_name` as sharing the
 * convention, so the siblings were never hidden.
 *
 * ## Why the Rust source and not a generated file
 *
 * A generator is the better answer and building one is not this test's job: it
 * needs a build step, a checked-in artefact and a CI check that the artefact is
 * current, which is three moving parts to keep four short lists honest. This
 * reads the source that already exists, in the suite that already runs.
 *
 * ## What it cannot catch, and why every extraction throws
 *
 * A rename of a Rust function, or a reshaped `match`, makes the patterns below
 * find nothing. A silent empty set would compare equal to a union nobody had
 * updated either, which is this test passing for the exact reason it exists. So
 * each step throws instead, and the failure modes are drilled rather than
 * assumed: an added arm fails the pair, renaming the function fails it with
 * "has it been renamed?", and a decoy `fn <name>(` planted *earlier* in the file
 * fails it with "has the match been reshaped?" rather than mis-extracting.
 *
 * It still cannot see a name that reaches the frontend by any route other than
 * these four functions.
 */

/** Every wire-name pair this file checks. */
const PAIRS = [
  { union: 'AreaKind', rustFn: 'type_name', rustEnum: 'AreaType' },
  { union: 'ArmableType', rustFn: 'armable_type', rustEnum: 'AreaType' },
  { union: 'LayerName', rustFn: 'layer_name', rustEnum: 'Layer' },
  { union: 'OverlayStateName', rustFn: 'state_name', rustEnum: 'OverlayState' },
] as const;

/**
 * A wire name as the regexes below accept it.
 *
 * Deliberately wider than the four lists need. It was `[a-z]+` first, and the
 * independent review drilled the hole: an added arm mapping to `"deep_zoom"` was
 * skipped on *both* sides, so the two sets still compared equal and the suite
 * stayed green on exactly the change this file exists to catch. A character
 * class that excludes a plausible future name is a check that cannot go red.
 */
const WIRE_NAME = '[a-z][a-z0-9_-]*';

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
 * Every wire name a `match` in `fn name` pairs with a variant of `enumName`.
 *
 * Both directions, because the functions face opposite ways: `type_name`,
 * `layer_name` and `state_name` map a variant to a string, while `armable_type`
 * maps a string to a variant.
 */
function wireNamesIn(name: string, enumName: string): string[] {
  const body = rustFunctionBody(name);
  const outbound = [
    ...body.matchAll(
      new RegExp(
        `${enumName}::\\w+\\s*=>\\s*(?:Some\\()?"(${WIRE_NAME})"`,
        'g',
      ),
    ),
  ].map((match) => match[1]);
  const inbound = [
    ...body.matchAll(
      new RegExp(`"(${WIRE_NAME})"\\s*=>\\s*Some\\(${enumName}::\\w+\\)`, 'g'),
    ),
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
  const members = [
    ...declaration[1].matchAll(new RegExp(`'(${WIRE_NAME})'`, 'g')),
  ].map((match) => match[1]);
  if (members.length === 0) {
    throw new Error(
      `extracted no members from ${name}: has it stopped naming its literals?`,
    );
  }
  return members;
}

const sorted = (values: string[]) => [...values].sort();

describe('the wire vocabulary agrees across the IPC boundary', () => {
  for (const { union, rustFn, rustEnum } of PAIRS) {
    it(`${union} lists exactly the names ${rustFn} handles`, () => {
      expect(sorted(literalsIn(union))).toEqual(
        sorted(wireNamesIn(rustFn, rustEnum)),
      );
    });
  }

  it('every armable name is also an AreaKind', () => {
    const kinds = literalsIn('AreaKind');
    for (const name of wireNamesIn('armable_type', 'AreaType')) {
      expect(kinds).toContain(name);
    }
  });

  it('no union repeats a name', () => {
    for (const { union } of PAIRS) {
      const members = literalsIn(union);
      expect(sorted(members)).toEqual(sorted([...new Set(members)]));
    }
  });
});
