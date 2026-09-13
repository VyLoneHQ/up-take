/**
 * Every word the first-run coach says (roadmap 1.18, ADR-0043).
 *
 * One file on purpose. ADR-0043 decision 5 makes the last step the single
 * source of the keybind reference that 1.18 wants in three places (the tour,
 * the in-app docs and the landing page), and `1.38` translates the product
 * from its strings. A sentence typed into the component instead would be a
 * second copy that neither of those can find.
 *
 * **The key names here are checked against the code that binds them**, in
 * `coach-copy.test.ts`: the two global shortcuts against `hotkey.rs`, and the
 * letters, `Ctrl+Space`, `Delete` and `Esc` against the handlers that act on
 * them. A reference sheet that named a key the app does not honour would be
 * the one screen the user trusts to be right.
 *
 * The copy is the approved mockups' (`Projects/UP-TAKE/mockups/`, 2026-09-11)
 * with three changes, each for a reason:
 *
 * - Step 3's closing line in Living said "Try clicking your editor", which was
 *   the mockup's own backdrop. The ordinary user has no editor open.
 * - Step 3 needed a closing line for Placement, which the mockups never drew.
 * - Step 4's settings button and its two sentences about Settings are left out
 *   until roadmap 1.14 builds Settings, so the tour never points at a window
 *   that does not exist.
 * - Step 2 names the `O` type with the product's own label, "OCR", where the
 *   mockup said "Text": the area menu and the type bar both say "OCR", and a
 *   tour that taught a different word for the same thing would be wrong on the
 *   first right-click. Found by the independent review of 1.18. Whether the
 *   product should say "Text" everywhere is a separate question.
 */

import { KIND_LABELS } from './overlay-state';

/** "Step 2 of 4". */
export function progress(step: number, total: number): string {
  return `Step ${step} of ${total}`;
}

/** The two buttons every step but the last carries, and the last step's one. */
export const BUTTONS = {
  next: 'Next',
  skip: 'Skip',
  finish: 'Start using UP-TAKE',
} as const;

/** Step 1. */
export const DRAW = {
  title: 'Drag anywhere to draw an area',
  body: 'An area is a rectangle that stays on your screen and does something with whatever is underneath it. Everything UP-TAKE does is an area, a property of an area, or an action on one.',
  prompt:
    'Go ahead and draw one now. This guide waits for you and moves on by itself.',
  escKey: 'Esc',
  escHint: 'leaves at any time',
} as const;

/** How a type row's swatch is coloured. */
export type TypeTone = 'accent' | 'filter' | 'upscale' | 'text';

/** One area type on step 2. */
export interface TypeRow {
  /** The key that arms it, as printed. */
  key: string;
  name: string;
  does: string;
  tone: TypeTone;
}

/** Step 2. */
export const TYPES = {
  title: 'Press a key first, then drag',
  body: 'The key decides what kind of area you get. The colour tells you which kind it is without you having to remember.',
  // The names are the product's own labels, read from `KIND_LABELS`, which the
  // type bar uses and which match the area menu's. The tour teaches what the
  // user will then see on the area itself, so it may not have a word of its own.
  rows: [
    {
      key: 'S',
      name: KIND_LABELS.screenshot,
      does: 'Pins a still of the region into the area',
      tone: 'accent',
    },
    {
      key: 'F',
      name: KIND_LABELS.filter,
      does: 'A warm tint you go on working underneath',
      tone: 'filter',
    },
    {
      key: 'U',
      name: KIND_LABELS.upscale,
      does: 'Re-takes the region and sharpens it in place',
      tone: 'upscale',
    },
    {
      key: 'O',
      name: KIND_LABELS.ocr,
      does: 'Reads the text in the region so you can copy it',
      tone: 'text',
    },
  ] satisfies TypeRow[],
  footer: 'Drag with no key held and you get a plain area.',
} as const;

/** Step 3. */
export const MODES = {
  title: 'Hand the screen back',
  body: 'Your areas do not need UP-TAKE to be in the way. One chord gives your clicks back to whatever is underneath, and the areas stay exactly where you put them.',
  chord: ['Win', 'Shift', 'U'],
  chordNote:
    'Switches between the two. Press it again whenever you want to place, move or remove an area.',
  placing: {
    name: 'Placing',
    does: 'A blue frame round the screen. Your clicks go to UP-TAKE, so you can draw and arrange.',
  },
  living: {
    name: 'Living',
    does: 'No frame. Your clicks go to your own apps. The areas keep doing their job.',
  },
  footerPlacing: 'Press the chord now to try it.',
  footerLiving:
    'You are in Living right now. Try clicking one of your apps, then press the chord again.',
} as const;

/** One line of the reference sheet. */
export interface KeyRow {
  keys: string;
  does: string;
}

/** Step 4, the reference sheet. */
export const REFERENCE = {
  title: 'That is the whole of it',
  body: 'Everything else is a menu on an area. Right-click any area to change its type, pin it above or below, or throw it away.',
  anywhereHeading: 'Anywhere',
  anywhere: [
    { keys: 'Win+Shift+U', does: 'Placing or Living' },
    { keys: 'Win+Shift+G', does: 'Copy this monitor' },
    { keys: 'Win+Shift+drag', does: 'Move an area from Living' },
  ] satisfies KeyRow[],
  placingHeading: 'While placing',
  placing: [
    { keys: 'drag', does: 'Draw an area' },
    { keys: 'S F U O', does: "Set the next area's type" },
    { keys: 'Ctrl+Space', does: 'Freeze the screen' },
    { keys: 'Delete', does: 'Throw an area away' },
    { keys: 'Esc', does: 'Leave placing' },
  ] satisfies KeyRow[],
} as const;
