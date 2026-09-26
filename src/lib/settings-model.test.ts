import { describe, expect, it } from 'vitest';
import { coachCopy } from './coach-copy';
import {
  allRows,
  type Facts,
  languageFor,
  PANE_IDS,
  type PaneId,
  panes,
  type Row,
  type Settings,
} from './settings-model';
import { LANGUAGES } from './strings';

/**
 * The settings window's content (roadmap 1.14).
 *
 * `UI-UX.md` section 4 calls itself *"the check-list 1.14 builds against"*, and
 * a check-list nothing runs is a description. This file is what runs it: the
 * table below is that section transcribed, and the first test fails if a row
 * moves pane, changes control shape, or goes missing.
 *
 * The section says why that matters in its own words: the roadmap's prose "has
 * already been wrong about its own count once", saying three settings while the
 * code and `ADR-0026`'s third amendment both said four.
 */

/**
 * `UI-UX.md` section 4, transcribed. The two hotkey rows are `fact` here.
 *
 * ⚠️ **Transcribed means transcribed, and for a while it did not.** The
 * auto-save row was added to this list when the founder asked for it on the rig
 * and to section 4 hours later. The document is the check-list; when the two
 * disagree, the document is what has to change first, or this constant is just
 * a second opinion.
 *
 * ⛔ **The first version of this paragraph said the document had already been
 * corrected, and it had not.** The edit existed in a working tree and was never
 * committed, while the commit carrying this comment asserted it as done. Round
 * 2 of the Sonnet 5 review read the planning repository at its real HEAD, found
 * the row absent, and returned REQUEST_CHANGES on that alone. It was right.
 * Landed for real in workspace `83d8512d`.
 *
 * **Nothing can check this, which is the actual defect.** The table and this
 * constant are in different repositories with no submodule, no shared CI and
 * no probe, so the sync is a rule somebody has to remember, and it survived six
 * hours. UP-TAKE backlog `I-413`.
 */
const INVENTORY: { pane: PaneId; id: string; shape: Row['shape'] }[] = [
  { pane: 'general', id: 'start-with-windows', shape: 'toggle' },
  { pane: 'general', id: 'hand-launch-state', shape: 'segmented' },
  { pane: 'general', id: 'hotkey-summon', shape: 'fact' },
  { pane: 'general', id: 'hotkey-grab', shape: 'fact' },
  { pane: 'capture', id: 'save-directory', shape: 'folder' },
  { pane: 'capture', id: 'auto-save', shape: 'toggle' },
  { pane: 'capture', id: 'leave-placing', shape: 'toggle' },
  { pane: 'capture', id: 'freeze-covers', shape: 'segmented' },
  { pane: 'capture', id: 'held-picture-quality', shape: 'segmented' },
  { pane: 'capture', id: 'show-in-recordings', shape: 'toggle' },
  { pane: 'appearance', id: 'area-opacity', shape: 'slider' },
  { pane: 'appearance', id: 'filter-strength', shape: 'slider' },
  { pane: 'appearance', id: 'language', shape: 'segmented' },
  { pane: 'ocr', id: 'ocr-behaviour', shape: 'segmented' },
];

/** The shipped defaults, which `settings.rs` asserts from the other side. */
const DEFAULTS: Settings = {
  start_with_windows: false,
  hand_launch_state: 'placing',
  save_directory: null,
  leave_placing_after_screenshot: false,
  auto_save_screenshots: false,
  freeze_covers: 'this_monitor',
  held_picture_quality: 'fast',
  show_in_screen_recordings: false,
  area_opacity_percent: 40,
  filter_strength_percent: 16,
  language: 'system',
  ocr_behaviour: 'in_place',
};

const FACTS: Facts = {
  summon_hotkey: 'Win+Shift+U',
  grab_hotkey: 'Win+Shift+G',
  default_save_directory: 'C:\\Users\\someone\\Pictures\\UP-TAKE',
  autostart_registered: false,
  opacity_range: [10, 100],
  filter_range: [5, 60],
  system_language: 'de',
  acrylic: true,
};

const view = (settings: Settings = DEFAULTS, facts: Facts = FACTS) =>
  panes('en', settings, facts);

function row(id: string, settings: Settings = DEFAULTS, facts: Facts = FACTS) {
  const found = allRows(view(settings, facts)).find((each) => each.id === id);
  if (!found) throw new Error(`no row ${id}`);
  return found;
}

describe('the inventory', () => {
  it('has every row UI-UX.md section 4 lists, in its pane and its shape', () => {
    const actual = view().flatMap((pane) =>
      pane.sections.flatMap((section) =>
        section.rows
          // The Help pane's reference sheet and its replay button are not
          // settings; section 4 lists Help as one row, "the keybind reference".
          .filter(
            (each) =>
              each.shape !== 'keys' &&
              each.shape !== 'action' &&
              each.shape !== 'types',
          )
          .map((each) => ({
            pane: pane.id,
            id: each.id,
            shape: each.shape,
          })),
      ),
    );
    expect(actual).toEqual(INVENTORY);
  });

  it('puts the keybind reference, the type legend, the replay and the reset in Help', () => {
    // ADR-0043 decision 5: the tour's last step is the single source of the
    // reference, "reachable afterwards from Settings, Help", and the ADR's
    // consequences add a way to replay the tour. The legend and the reset are
    // the founder's, from the rig on 2026-09-17.
    const help = view().find((pane) => pane.id === 'help');
    const shapes = help?.sections.flatMap((section) =>
      section.rows.map((each) => each.shape),
    );
    expect(shapes).toEqual(['keys', 'keys', 'types', 'action', 'action']);
  });

  it('drops the arming row from Help, because the legend replaced it', () => {
    // The founder on the rig: `S F U O -- Set the next area's type` is
    // "not good". The legend below it now answers that properly, so the Help
    // pane must not ask the same question twice, badly first.
    const keys = row('keys-placing');
    if (keys.shape !== 'keys') throw new Error('not the key sheet');
    const does = keys.keys.map((each) => each.does);

    expect(does).not.toContain("Set the next area's type");
    expect(keys.keys.map((each) => each.keys)).not.toContain('S F U O');
    // Everything else survives. A filter that matched too much would leave a
    // reference sheet quietly missing keys the app still honours.
    expect(does).toEqual([
      'Draw an area',
      'Freeze the screen',
      'Throw an area away',
      'Leave placing',
    ]);
  });

  it('keeps the arming row on the TOUR, which is a different question', () => {
    // On the tour that line reminds the user of the step they were just walked
    // through, with the four types on the screen behind it. Filtering it there
    // too would be this fix overreaching.
    const tour = coachCopy('en').REFERENCE.placing.map((each) => each.does);
    expect(tour).toContain("Set the next area's type");
  });

  it('builds the type legend from the tour rather than a second list', () => {
    // The founder's words on the rig: the reference sheet's `S F U O` row
    // "is not good". It names four keys and says what none of them does.
    // The legend is step 2 of the tour, whose key names `coach-copy.test.ts`
    // already checks against the handlers that bind them -- so reading it from
    // there inherits that check instead of starting a second vocabulary.
    const legend = row('type-legend');
    if (legend.shape !== 'types') throw new Error('not the legend');

    // Default first, because it is the one type the tour does not teach: it
    // has no key and it is what a plain drag gives you.
    expect(legend.types.map((each) => each.key)).toEqual([
      'no key',
      'S',
      'F',
      'U',
      'O',
    ]);
    // Every row says what the type DOES, which is the whole complaint.
    for (const each of legend.types) {
      expect(each.does.length, each.name).toBeGreaterThan(10);
      expect(each.name).toBeTruthy();
    }
    // Per-type colour carries information (UI-UX.md section 2), so no two
    // types that are told apart by colour may share a tone by accident.
    expect(new Set(legend.types.map((each) => each.tone)).size).toBeGreaterThan(
      1,
    );
  });

  it('offers a reset that does not carry its own copy of the defaults', () => {
    // The row is an action, not a settings row: the values come from Rust's
    // `settings_defaults`. A list on this side would stop matching
    // `Settings::default` the first time a default changed, and Reset would
    // put the user back to something that was never shipped.
    const reset = row('reset-defaults');
    if (reset.shape !== 'action') throw new Error('not an action');
    expect(reset.action).toBe('reset-defaults');
    expect(JSON.stringify(reset)).not.toContain('area_opacity_percent');
  });

  it('names the panes in the sidebar order section 3.2 gives', () => {
    expect(view().map((pane) => pane.id)).toEqual([...PANE_IDS]);
    expect(PANE_IDS).toEqual([
      'general',
      'capture',
      'appearance',
      'ocr',
      'help',
    ]);
  });

  it('reads the keybind rows from the tour rather than restating them', () => {
    // A second copy of "Win+Shift+U does this" is the F-22/F-37 failure, and
    // `coach-copy.ts` checks its key names against the handlers that bind
    // them. Reading from there is what inherits that check.
    const summon = row('hotkey-summon');
    expect(summon.shape).toBe('fact');
    if (summon.shape !== 'fact') return;
    expect(summon.value).toBe(FACTS.summon_hotkey);

    const keys = row('keys-anywhere');
    if (keys.shape !== 'keys') throw new Error('not the reference sheet');
    expect(keys.keys.map((each) => each.keys)).toEqual([
      'Win+Shift+U',
      'Win+Shift+G',
      'Win+Shift+drag',
    ]);
  });
});

describe('a control', () => {
  it('returns a new settings object and changes exactly one field', () => {
    const toggle = row('leave-placing');
    if (toggle.shape !== 'toggle') throw new Error('not a toggle');
    const next = toggle.set(true);

    expect(next).not.toBe(DEFAULTS);
    expect(DEFAULTS.leave_placing_after_screenshot).toBe(false);
    expect(next.leave_placing_after_screenshot).toBe(true);
    expect({ ...next, leave_placing_after_screenshot: false }).toEqual(
      DEFAULTS,
    );
  });

  it('shows the value it was given, not a default of its own', () => {
    const changed: Settings = {
      ...DEFAULTS,
      freeze_covers: 'every_monitor',
      area_opacity_percent: 75,
      language: 'german',
    };
    const covers = row('freeze-covers', changed);
    const opacity = row('area-opacity', changed);
    const language = row('language', changed);
    if (covers.shape !== 'segmented') throw new Error('not segmented');
    if (opacity.shape !== 'slider') throw new Error('not a slider');
    if (language.shape !== 'segmented') throw new Error('not segmented');

    expect(covers.value).toBe('every_monitor');
    expect(opacity.value).toBe(75);
    expect(language.value).toBe('german');
  });

  it('takes its slider bounds from Rust rather than repeating them', () => {
    // `settings.rs` owns the ranges and clamps to them. A second copy here
    // would let the slider offer a value the store then silently changes.
    const opacity = row('area-opacity');
    const filter = row('filter-strength');
    if (opacity.shape !== 'slider' || filter.shape !== 'slider') {
      throw new Error('not sliders');
    }
    expect([opacity.min, opacity.max]).toEqual(FACTS.opacity_range);
    expect([filter.min, filter.max]).toEqual(FACTS.filter_range);
    // The floor is not zero: an area at 0% has no border to grab and no
    // chrome to right-click, so it could not be recovered outside Placement.
    expect(opacity.min).toBeGreaterThan(0);
  });

  it('offers every wire value Rust accepts, and no other', () => {
    // A segment whose value Rust does not deserialize is a control that looks
    // right and does nothing. These are `settings.rs`'s `rename_all` outputs,
    // which its own test pins from the other side.
    const segments = (id: string) => {
      const found = row(id);
      if (found.shape !== 'segmented')
        throw new Error(`${id} is not segmented`);
      return found.segments.map((each) => each.value);
    };
    expect(segments('hand-launch-state')).toEqual(['placing', 'hidden']);
    expect(segments('freeze-covers')).toEqual([
      'this_monitor',
      'every_monitor',
    ]);
    expect(segments('held-picture-quality')).toEqual(['fast', 'exact']);
    expect(segments('language')).toEqual(['system', 'english', 'german']);
    expect(segments('ocr-behaviour')).toEqual(['in_place', 'rendered']);
  });

  it('empties the save folder back to the default rather than to a path', () => {
    const folder = row('save-directory');
    if (folder.shape !== 'folder') throw new Error('not a folder');
    // Empty means "use my Pictures folder", and that has to reach Rust as
    // `null`: a stored empty string would be a folder named nothing.
    expect(folder.value).toBe('');
    expect(folder.placeholder).toBe(FACTS.default_save_directory);
    expect(folder.set(null).save_directory).toBeNull();
    expect(folder.set('D:\\Shots').save_directory).toBe('D:\\Shots');
    // Open folder is a label on this row rather than a row of its own: it acts
    // on the folder the row is about. Asked for on the rig, 2026-09-17.
    expect(folder.open).toBeTruthy();
  });
});

describe('the startup switch', () => {
  it('says nothing when the setting and the machine agree', () => {
    const off = row('start-with-windows');
    if (off.shape !== 'toggle') throw new Error('not a toggle');
    expect(off.warning).toBeUndefined();

    const on = row(
      'start-with-windows',
      { ...DEFAULTS, start_with_windows: true },
      {
        ...FACTS,
        autostart_registered: true,
      },
    );
    if (on.shape !== 'toggle') throw new Error('not a toggle');
    expect(on.warning).toBeUndefined();
  });

  it('says so when it is on and the entry is not on this machine', () => {
    // A copied profile, or a startup cleaner. Reported rather than silently
    // rewritten: writing a Run key the user did not just ask for is the one
    // thing a startup switch must not do.
    const on = row('start-with-windows', {
      ...DEFAULTS,
      start_with_windows: true,
    });
    if (on.shape !== 'toggle') throw new Error('not a toggle');
    expect(on.warning).toBeTruthy();
  });
});

describe('the words', () => {
  it('are present in every language the catalogue ships', () => {
    // `strings.test.ts` proves the catalogue is complete; this proves the
    // window asks it for keys that exist, in every language, rather than
    // rendering a key name at someone whose language is not English.
    for (const language of LANGUAGES) {
      for (const pane of panes(language, DEFAULTS, FACTS)) {
        expect(pane.name, `${language}: pane ${pane.id}`).not.toMatch(
          /^settings\./,
        );
        for (const section of pane.sections) {
          for (const each of section.rows) {
            if (each.name) {
              expect(each.name, `${language}: ${each.id}`).not.toMatch(
                /^settings\./,
              );
            }
            if (each.about) {
              expect(each.about, `${language}: ${each.id} about`).not.toMatch(
                /^settings\./,
              );
            }
          }
        }
      }
    }
  });

  it('differ between English and German, so nothing is left untranslated', () => {
    const names = (language: 'en' | 'de') =>
      panes(language, DEFAULTS, FACTS).map((pane) => pane.name);
    expect(names('de')).not.toEqual(names('en'));
    expect(names('de')).toEqual([
      'Allgemein',
      'Aufnahme',
      'Darstellung',
      'OCR',
      'Hilfe',
    ]);
  });
});

describe('the language row', () => {
  it('resolves a choice to the language the window re-renders in', () => {
    // The review of PR #105 found this row was the weakest of the ten: it
    // stored a value and nothing in the running process changed. The window
    // itself can change, and does. The overlay and the native menus cannot --
    // Rust fixes its language once per process and hands out &'static str --
    // and the row's own sentence says which is which.
    expect(languageFor('english', FACTS)).toBe('en');
    expect(languageFor('german', FACTS)).toBe('de');
  });

  it('asks Rust what Windows says rather than guessing', () => {
    // `system` cannot be resolved on this side, and it is NOT the language the
    // process is running in: those part company the moment somebody changes
    // the setting, which is exactly when this is needed.
    expect(languageFor('system', FACTS)).toBe(FACTS.system_language);
    expect(languageFor('system', { ...FACTS, system_language: 'en' })).toBe(
      'en',
    );
  });
});
