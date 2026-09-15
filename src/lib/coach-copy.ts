/**
 * Every word the first-run coach says (roadmap 1.18, ADR-0043), in the user's
 * language (roadmap 1.38).
 *
 * One function on purpose. ADR-0043 decision 5 makes the last step the single
 * source of the keybind reference that 1.18 wants in three places (the tour,
 * the in-app docs and the landing page). The sentences themselves live in
 * `locales/strings.json` beside their translations; this file gives them the
 * shape the coach draws, so a sentence typed into the component instead would
 * be a copy neither the catalogue nor its checks can find.
 *
 * **The key names here are checked against the code that binds them**, in
 * `coach-copy.test.ts`: the two global shortcuts against `hotkey.rs`, and the
 * letters, `Ctrl+Space`, `Delete` and `Esc` against the handlers that act on
 * them. A reference sheet that named a key the app does not honour would be
 * the one screen the user trusts to be right. They are printed as they are
 * written on the keys the handlers match, in every language; whether a German
 * sheet should say `Strg` and `Entf` is an open question, and deciding it means
 * checking those names against the handlers too.
 *
 * The copy is the approved mockups' (`Projects/UP-TAKE/mockups/`, 2026-09-11)
 * with these changes, each for a reason:
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

import { kindLabels } from './overlay-state';
import { fill, type Language, type TextKey, text } from './strings';

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

/** One line of the reference sheet. */
export interface KeyRow {
  keys: string;
  does: string;
}

/** Everything the coach says, in one language. */
export function coachCopy(language: Language) {
  const say = (key: TextKey) => text(language, key);
  // The names are the product's own labels, read from `kindLabels`, which the
  // type bar uses and which match the area menu's. The tour teaches what the
  // user will then see on the area itself, so it may not have a word of its own.
  const kinds = kindLabels(language);
  const drag = say('coach.reference.drag_key');

  return {
    /** "Step 2 of 4". */
    progress: (step: number, total: number): string =>
      fill(language, 'coach.progress', { step, total }),

    /** The two buttons every step but the last carries, and the last step's one. */
    BUTTONS: {
      next: say('coach.next'),
      skip: say('coach.skip'),
      finish: say('coach.finish'),
    },

    /** Step 1. */
    DRAW: {
      title: say('coach.draw.title'),
      body: say('coach.draw.body'),
      prompt: say('coach.draw.prompt'),
      escKey: 'Esc',
      escHint: say('coach.draw.esc_hint'),
    },

    /** Step 2. */
    TYPES: {
      title: say('coach.types.title'),
      body: say('coach.types.body'),
      rows: [
        {
          key: 'S',
          name: kinds.screenshot,
          does: say('coach.types.screenshot'),
          tone: 'accent',
        },
        {
          key: 'F',
          name: kinds.filter,
          does: say('coach.types.filter'),
          tone: 'filter',
        },
        {
          key: 'U',
          name: kinds.upscale,
          does: say('coach.types.upscale'),
          tone: 'upscale',
        },
        {
          key: 'O',
          name: kinds.ocr,
          does: say('coach.types.ocr'),
          tone: 'text',
        },
      ] satisfies TypeRow[],
      footer: say('coach.types.footer'),
    },

    /** Step 3. */
    MODES: {
      title: say('coach.modes.title'),
      body: say('coach.modes.body'),
      chord: ['Win', 'Shift', 'U'],
      chordNote: say('coach.modes.chord_note'),
      placing: {
        name: say('coach.modes.placing.name'),
        does: say('coach.modes.placing.does'),
      },
      living: {
        name: say('coach.modes.living.name'),
        does: say('coach.modes.living.does'),
      },
      footerPlacing: say('coach.modes.footer_placing'),
      footerLiving: say('coach.modes.footer_living'),
    },

    /** Step 4, the reference sheet. */
    REFERENCE: {
      title: say('coach.reference.title'),
      body: say('coach.reference.body'),
      anywhereHeading: say('coach.reference.anywhere'),
      anywhere: [
        { keys: 'Win+Shift+U', does: say('coach.reference.summon') },
        { keys: 'Win+Shift+G', does: say('coach.reference.grab') },
        {
          keys: `Win+Shift+${drag}`,
          does: say('coach.reference.move_living'),
        },
      ] satisfies KeyRow[],
      placingHeading: say('coach.reference.placing'),
      placing: [
        { keys: drag, does: say('coach.reference.draw') },
        { keys: 'S F U O', does: say('coach.reference.type') },
        { keys: 'Ctrl+Space', does: say('coach.reference.freeze') },
        { keys: 'Delete', does: say('coach.reference.remove') },
        { keys: 'Esc', does: say('coach.reference.leave') },
      ] satisfies KeyRow[],
    },
  } as const;
}

/** The coach's words in one language. */
export type CoachCopy = ReturnType<typeof coachCopy>;
