/**
 * The settings window's content, as data (roadmap 1.14, `UI-UX.md` sections 3.2
 * and 4).
 *
 * The component draws whatever this returns. That split is the same one
 * `coach-copy.ts` makes and for the same reason: the interesting part of a
 * settings window is *which rows exist, in which pane, with which control and
 * which default*, and none of that needs a DOM to check. `settings-model.test.ts`
 * asserts this against `UI-UX.md` section 4's table row by row, which is what
 * makes that table a check-list rather than a description.
 *
 * # Every row is pure
 *
 * A control does not mutate anything. It calls its row's `set`, which returns a
 * **new** `Settings`, and the component sends that to Rust. So a test can drive
 * a row the way a user would and read the result, with no component and no IPC.
 *
 * # The shapes, and the one that is not in the approved list
 *
 * `UI-UX.md` section 3.2 says three: a toggle, a segmented choice of two or
 * three, and a slider. It adds that needing a fourth is a sign the setting is
 * too complicated for the window.
 *
 * **Section 4's own inventory needs more than three**, and that is a
 * contradiction between two halves of one approved document rather than a
 * liberty taken here:
 *
 * - *Save pictures to* is a folder. No toggle, segment or slider can express a
 *   path, and the row is in section 4.
 * - The two *Hotkey* rows are described as rebindable, which needs a control
 *   that captures a keystroke. **They are read-only here** and the question is
 *   the founder's, so this file ships a `fact` shape rather than inventing the
 *   capture control section 3.2 warns against.
 * - The *Help* pane holds a reference sheet and a button, neither of which is a
 *   setting at all.
 *
 * So: three shapes for anything that stores a value, plus `folder`, plus `fact`
 * and `action` for the rows that are not settings. Recorded here rather than
 * settled here.
 */

import { coachCopy, type KeyRow, type TypeRow } from './coach-copy';
import { kindLabels } from './overlay-state';
import { type Language, type TextKey, text } from './strings';

/** The four panes, in the sidebar's order (`UI-UX.md` section 3.2). */
export const PANE_IDS = ['general', 'capture', 'appearance', 'help'] as const;

/** Which pane is showing. */
export type PaneId = (typeof PANE_IDS)[number];

/** Where a launch by hand lands. Rust's `HandLaunchState`, on the wire. */
export type HandLaunchState = 'placing' | 'hidden';

/** Which monitors a freeze covers. Rust's `FreezeCovers`, on the wire. */
export type FreezeCovers = 'this_monitor' | 'every_monitor';

/** How a held picture is encoded. Rust's `HeldPictureQuality`, on the wire. */
export type HeldPictureQuality = 'fast' | 'exact';

/** Which language the interface shows. Rust's `Language`, on the wire. */
export type LanguageChoice = 'system' | 'english' | 'german';

/**
 * Everything the user can change.
 *
 * **Snake case, verbatim from Rust.** `settings.rs`'s own key test pins these
 * names from the other side, so a field renamed on one side fails there rather
 * than silently reading `undefined` here (`UT-F-72`'s class, and `I-67`).
 */
export interface Settings {
  start_with_windows: boolean;
  hand_launch_state: HandLaunchState;
  save_directory: string | null;
  leave_placing_after_screenshot: boolean;
  auto_save_screenshots: boolean;
  freeze_covers: FreezeCovers;
  held_picture_quality: HeldPictureQuality;
  show_in_screen_recordings: boolean;
  area_opacity_percent: number;
  filter_strength_percent: number;
  language: LanguageChoice;
}

/** What the window shows and cannot change. Rust's `Facts`. */
export interface Facts {
  summon_hotkey: string;
  grab_hotkey: string;
  default_save_directory: string;
  autostart_registered: boolean;
  opacity_range: [number, number];
  filter_range: [number, number];
  system_language: string;
  acrylic: boolean;
}

/**
 * The catalogue language a Language choice means, given what Windows says.
 *
 * The window re-renders itself with this the moment the row is used, so the
 * setting does something where the user is looking rather than only at the next
 * launch. The overlay and the native menus still wait for a restart, and the
 * row's own sentence says so -- `strings.rs` explains why in one place.
 */
export function languageFor(choice: LanguageChoice, facts: Facts): string {
  if (choice === 'english') return 'en';
  if (choice === 'german') return 'de';
  return facts.system_language;
}

/** One option of a segmented control. */
export interface Segment<T extends string> {
  value: T;
  label: string;
}

interface Common {
  /** Stable across languages, so a test and a DOM query can name a row. */
  id: string;
  name: string;
  /** The sentence of plain description under the name. May be empty. */
  about: string;
}

/** A row the user can change, or read. */
export type Row =
  | (Common & {
      shape: 'toggle';
      value: boolean;
      set: (value: boolean) => Settings;
      /** Shown under the row when it is on and the machine disagrees. */
      warning?: string;
    })
  | (Common & {
      shape: 'segmented';
      value: string;
      segments: Segment<string>[];
      set: (value: string) => Settings;
    })
  | (Common & {
      shape: 'slider';
      value: number;
      min: number;
      max: number;
      set: (value: number) => Settings;
    })
  | (Common & {
      shape: 'folder';
      /** The chosen folder, or empty when the default is in use. */
      value: string;
      /** What the default resolves to, shown when `value` is empty. */
      placeholder: string;
      choose: string;
      reset: string;
      /** Opens it in Explorer. Asked for on the rig, 2026-09-17. */
      open: string;
      set: (value: string | null) => Settings;
    })
  | (Common & { shape: 'fact'; value: string })
  | (Common & {
      shape: 'action';
      label: string;
      action: 'replay-tour' | 'reset-defaults';
    })
  | (Common & { shape: 'keys'; heading: string; keys: readonly KeyRow[] })
  | (Common & {
      shape: 'types';
      heading: string;
      /** One per area type, with the key that arms it and its colour. */
      types: readonly TypeRow[];
    });

/** A labelled group of rows. */
export interface Section {
  /** The 11px uppercase label, or empty for an unlabelled group. */
  label: string;
  rows: Row[];
}

/** One pane of the window. */
export interface Pane {
  id: PaneId;
  name: string;
  sections: Section[];
}

/**
 * Every pane, for one language and one set of values.
 *
 * `settings` is never mutated. Each row's `set` returns a fresh object, so the
 * caller decides what to do with it.
 */
export function panes(
  language: Language,
  settings: Settings,
  facts: Facts,
): Pane[] {
  const say = (key: TextKey) => text(language, key);
  const with_ = (patch: Partial<Settings>): Settings => ({
    ...settings,
    ...patch,
  });
  const copy = coachCopy(language);
  const reference = copy.REFERENCE;
  const types = copy.TYPES;
  const kinds = kindLabels(language);

  return [
    {
      id: 'general',
      name: say('settings.pane.general'),
      sections: [
        {
          label: say('settings.section.startup'),
          rows: [
            {
              shape: 'toggle',
              id: 'start-with-windows',
              name: say('settings.start_with_windows.name'),
              about: say('settings.start_with_windows.about'),
              value: settings.start_with_windows,
              set: (value) => with_({ start_with_windows: value }),
              // The stored setting says yes and the machine says no. Shown
              // rather than silently repaired: writing the entry behind the
              // user's back is the one thing a startup switch must not do, and
              // the honest cases for this are a copied profile and a cleaner
              // having removed it.
              warning:
                settings.start_with_windows && !facts.autostart_registered
                  ? say('settings.start_with_windows.missing')
                  : undefined,
            },
            {
              shape: 'segmented',
              id: 'hand-launch-state',
              name: say('settings.hand_launch.name'),
              about: say('settings.hand_launch.about'),
              value: settings.hand_launch_state,
              segments: [
                {
                  value: 'placing',
                  label: say('settings.hand_launch.placing'),
                },
                { value: 'hidden', label: say('settings.hand_launch.hidden') },
              ],
              set: (value) =>
                with_({ hand_launch_state: value as HandLaunchState }),
            },
          ],
        },
        {
          label: say('settings.section.shortcuts'),
          rows: [
            {
              shape: 'fact',
              id: 'hotkey-summon',
              // The reference sheet's own wording, not a second one. ADR-0043
              // decision 5 makes that sheet the single source of what a key
              // does, in three places; this is the third.
              name: reference.anywhere[0].does,
              about: say('settings.hotkey.fixed'),
              value: facts.summon_hotkey,
            },
            {
              shape: 'fact',
              id: 'hotkey-grab',
              name: reference.anywhere[1].does,
              about: '',
              value: facts.grab_hotkey,
            },
          ],
        },
      ],
    },
    {
      id: 'capture',
      name: say('settings.pane.capture'),
      sections: [
        {
          label: say('settings.section.saving'),
          rows: [
            {
              shape: 'folder',
              id: 'save-directory',
              name: say('settings.save_to.name'),
              about: say('settings.save_to.about'),
              value: settings.save_directory ?? '',
              placeholder: facts.default_save_directory,
              choose: say('settings.save_to.choose'),
              reset: say('settings.save_to.reset'),
              open: say('settings.save_to.open'),
              set: (value) => with_({ save_directory: value }),
            },
            {
              shape: 'toggle',
              id: 'auto-save',
              name: say('settings.auto_save.name'),
              about: say('settings.auto_save.about'),
              value: settings.auto_save_screenshots,
              set: (value) => with_({ auto_save_screenshots: value }),
            },
            {
              shape: 'toggle',
              id: 'leave-placing',
              name: say('settings.leave_placing.name'),
              about: say('settings.leave_placing.about'),
              value: settings.leave_placing_after_screenshot,
              set: (value) => with_({ leave_placing_after_screenshot: value }),
            },
          ],
        },
        {
          label: say('settings.section.freezing'),
          rows: [
            {
              shape: 'segmented',
              id: 'freeze-covers',
              name: say('settings.freeze_covers.name'),
              about: say('settings.freeze_covers.about'),
              value: settings.freeze_covers,
              segments: [
                {
                  value: 'this_monitor',
                  label: say('settings.freeze_covers.this'),
                },
                {
                  value: 'every_monitor',
                  label: say('settings.freeze_covers.every'),
                },
              ],
              set: (value) => with_({ freeze_covers: value as FreezeCovers }),
            },
            {
              shape: 'segmented',
              id: 'held-picture-quality',
              name: say('settings.quality.name'),
              about: say('settings.quality.about'),
              value: settings.held_picture_quality,
              segments: [
                { value: 'fast', label: say('settings.quality.fast') },
                { value: 'exact', label: say('settings.quality.exact') },
              ],
              set: (value) =>
                with_({ held_picture_quality: value as HeldPictureQuality }),
            },
          ],
        },
        {
          label: say('settings.section.privacy'),
          rows: [
            {
              shape: 'toggle',
              id: 'show-in-recordings',
              name: say('settings.recordings.name'),
              about: say('settings.recordings.about'),
              value: settings.show_in_screen_recordings,
              set: (value) => with_({ show_in_screen_recordings: value }),
            },
          ],
        },
      ],
    },
    {
      id: 'appearance',
      name: say('settings.pane.appearance'),
      sections: [
        {
          label: say('settings.section.areas'),
          rows: [
            {
              shape: 'slider',
              id: 'area-opacity',
              name: say('settings.opacity.name'),
              about: say('settings.opacity.about'),
              value: settings.area_opacity_percent,
              min: facts.opacity_range[0],
              max: facts.opacity_range[1],
              set: (value) => with_({ area_opacity_percent: value }),
            },
            {
              shape: 'slider',
              id: 'filter-strength',
              name: say('settings.filter.name'),
              about: say('settings.filter.about'),
              value: settings.filter_strength_percent,
              min: facts.filter_range[0],
              max: facts.filter_range[1],
              set: (value) => with_({ filter_strength_percent: value }),
            },
          ],
        },
        {
          label: say('settings.section.language'),
          rows: [
            {
              shape: 'segmented',
              id: 'language',
              name: say('settings.language.name'),
              about: say('settings.language.about'),
              value: settings.language,
              segments: [
                { value: 'system', label: say('settings.language.system') },
                { value: 'english', label: say('settings.language.english') },
                { value: 'german', label: say('settings.language.german') },
              ],
              set: (value) => with_({ language: value as LanguageChoice }),
            },
          ],
        },
      ],
    },
    {
      id: 'help',
      name: say('settings.pane.help'),
      sections: [
        {
          label: '',
          rows: [
            // ADR-0043 decision 5: the tour's last step is the single source of
            // the keybind reference, "reachable afterwards from Settings, Help".
            // These rows are that sheet, read from `coachCopy` rather than
            // rewritten here, so a key that changes changes in one place.
            {
              shape: 'keys',
              id: 'keys-anywhere',
              name: '',
              about: say('settings.help.about'),
              heading: reference.anywhereHeading,
              keys: reference.anywhere,
            },
            {
              shape: 'keys',
              id: 'keys-placing',
              name: '',
              about: '',
              heading: reference.placingHeading,
              keys: reference.placing,
            },
          ],
        },
        {
          label: '',
          rows: [
            // The founder, on the rig 2026-09-17: the reference sheet's
            // `S F U O -- Set the next area's type` row *"is not good"*. It
            // names four keys and says nothing about what any of them gives
            // you, which is fine as the last line of a tour somebody has just
            // been walked through and useless as the thing they come back to.
            //
            // So the legend is the TOUR'S OWN step 2, read from `coachCopy`
            // rather than written again here: the same keys, the same product
            // labels from `kindLabels`, the same sentences, the same per-type
            // colour. A second vocabulary for the same five types is the
            // F-22/F-37 failure, and this is the third place ADR-0043
            // decision 5 says that content has to appear.
            //
            // Default is added in front, because it is the one type the tour
            // does not teach -- it has no key, it is what a plain drag gives
            // you, and a legend that omitted it would be a legend of the
            // exceptions.
            {
              shape: 'types',
              id: 'type-legend',
              name: '',
              about: say('settings.help.types.about'),
              heading: say('settings.help.types'),
              types: [
                {
                  key: say('settings.help.types.nokey'),
                  name: kinds.default,
                  does: say('settings.help.types.default'),
                  tone: 'accent',
                },
                ...types.rows,
              ],
            },
          ],
        },
        {
          label: say('settings.section.learning'),
          rows: [
            {
              shape: 'action',
              id: 'replay-tour',
              name: say('settings.help.replay.name'),
              about: say('settings.help.replay.about'),
              label: say('settings.help.replay.action'),
              action: 'replay-tour',
            },
          ],
        },
        {
          label: say('settings.section.reset'),
          rows: [
            // In Help rather than General, and last: it is the row somebody
            // looks for when something is wrong, which is where they already
            // are, and putting it under General would make it the second thing
            // anyone sees on opening the window.
            {
              shape: 'action',
              id: 'reset-defaults',
              name: say('settings.reset.name'),
              about: say('settings.reset.about'),
              label: say('settings.reset.action'),
              action: 'reset-defaults',
            },
          ],
        },
      ],
    },
  ];
}

/** Every row of every pane, for the tests that count them. */
export function allRows(list: Pane[]): Row[] {
  return list.flatMap((pane) =>
    pane.sections.flatMap((section) => section.rows),
  );
}
