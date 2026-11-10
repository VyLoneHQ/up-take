/**
 * The area settings panel (roadmap 1.60), the page side of
 * `src-tauri/src/panel.rs`. Rust opens, places and closes the panel's window
 * beside its area; the page reads the area with `panel_view` whenever
 * `panel://changed` arrives and sets it through the `panel_set_*` commands.
 */

/** What a watch area tells the user about. */
export type PanelWatch = 'changes' | 'settles';

/** Which of a watch area's events become a card (roadmap 1.65). */
export type PanelNotify = 'never' | 'once' | 'each';

/** The notification choices, in the order the panel shows them. */
export const NOTIFY: readonly PanelNotify[] = ['never', 'once', 'each'];

/** How a watch area reads and reports, as Rust's `Tuning` sends it. */
export interface PanelTuning {
  /** Which events become a card. */
  notify: PanelNotify;
  /** For `once`: how long without an event ends a burst, in ms. */
  calm_ms: number;
  /** The minimum gap after an event before the next may fire; 0 is none. */
  gap_ms: number;
  /** The least time between two readings; 0 is realtime. */
  sample_ms: number;
  /** The tolerance (roadmap 1.61): the share of the area, in thousandths,
   * that must change before it counts; 0 is any change, 1000 the whole area. */
  share_permille: number;
  /** For "stops changing": how long the area must hold still, in ms. */
  settle_ms: number;
}

/** The minimum gaps the panel offers, in ms: none, 0.1 s, 1 s, 10 s, 1 min. */
export const GAPS_MS: readonly number[] = [0, 100, 1_000, 10_000, 60_000];

/** A slider over a range of values, the same way for every slider (the
 * maintainer, 2026-11-10): from the lowest valid value to the highest, each
 * step 1% of the range and 10% while Ctrl is held (*"Make the default
 * increment (step-size) 1% (leave ctrl at 10x)"*, and *"I want every slider
 * to be able to be adjusted in both modes"*). The input itself runs from 0
 * to [`STEPS`]; a value is `min` plus that share of the range, rounded to
 * `round`. `reversed` runs the values from `max` on the left to `min` on
 * the right. */
export interface SliderRange {
  min: number;
  max: number;
  /** What a value is rounded to, so the panel shows 150 ms, not 149.6. */
  round: number;
  reversed?: boolean;
  /** Values above `min` and below this snap to `min`: for the sampling
   * slider, a step shorter than one captured frame reads no faster than
   * realtime, so it is shown and kept as realtime. */
  snap?: number;
}

/** How many steps a slider has: one per percent of its range. */
export const STEPS = 100;

/** How many steps one step is while Ctrl is held. */
export const COARSE = 10;

/** The fastest a watch area reads: one captured frame, about 30 a second
 * (`uptake-capture`'s `WATCH_FRAME_INTERVAL`, which throttles the capture
 * to save cost). Not the monitor's refresh rate. */
export const FRAME_MS = 34;

/** The calm slider, in ms: 50 ms to 10 s. Not from 0: a calm shorter than
 * one captured frame (about 34 ms) is no calm at all, so *Once* would do
 * what *Every time* does (the maintainer, 2026-11-10: *"0ms is NOT valid in
 * this case ... the lowest practical time would be probably around 25 -
 * 50ms"*). */
export const CALM_RANGE: SliderRange = { min: 50, max: 10_000, round: 10 };

/** The sampling slider, in ms between readings: every five seconds on the
 * left to realtime (0) on the right. A step shorter than a frame is
 * realtime (the maintainer, 2026-11-10: the label is capped at what the
 * reading can reach). */
export const SAMPLE_RANGE: SliderRange = {
  min: 0,
  max: 5_000,
  round: 10,
  reversed: true,
  snap: FRAME_MS,
};

/** The tolerance slider, in thousandths of the area: any change on the
 * left to the whole area on the right. */
export const SHARE_RANGE: SliderRange = { min: 0, max: 1_000, round: 10 };

/** The "stops changing" slider, in ms of stillness: 50 ms to 30 s. Not
 * from 0: while a change settles the area is read every 50 ms
 * (`SETTLE_POLL`), so a shorter calm waits the same 50 ms. */
export const SETTLE_RANGE: SliderRange = { min: 50, max: 30_000, round: 10 };

/** The input's own bounds: 0 to [`STEPS`] for every slider. */
export function track(_range: SliderRange): { min: number; max: number } {
  return { min: 0, max: STEPS };
}

/** The input's step: one percent, or ten while Ctrl is held. */
export function stepOf(_range: SliderRange, coarse: boolean): number {
  return coarse ? COARSE : 1;
}

/** Where `value` sits on the input, the nearest step, kept in range. */
export function toPosition(range: SliderRange, value: number): number {
  const kept = Math.min(range.max, Math.max(range.min, value));
  const share = (kept - range.min) / (range.max - range.min);
  const step = Math.round(share * STEPS);
  return range.reversed ? STEPS - step : step;
}

/** The value at input position `position`, rounded and kept in range. */
export function fromPosition(range: SliderRange, position: number): number {
  const step = Math.min(STEPS, Math.max(0, position));
  const share = (range.reversed ? STEPS - step : step) / STEPS;
  const raw = range.min + share * (range.max - range.min);
  const rounded = Math.round(raw / range.round) * range.round;
  const kept = Math.min(range.max, Math.max(range.min, rounded));
  return range.snap !== undefined && kept < range.snap ? range.min : kept;
}

/** How far along the track `value` sits, 0 at the left and 1 at the right,
 * for the drawn fill. */
export function fraction(range: SliderRange, value: number): number {
  return toPosition(range, value) / STEPS;
}

const isCount = (value: unknown): value is number =>
  typeof value === 'number' && Number.isInteger(value) && value >= 0;

/** Whether a value from Rust is a watch area's tuning. */
export function isPanelTuning(value: unknown): value is PanelTuning {
  if (typeof value !== 'object' || value === null) return false;
  const tuning = value as Record<string, unknown>;
  return (
    typeof tuning.notify === 'string' &&
    (NOTIFY as readonly string[]).includes(tuning.notify) &&
    isCount(tuning.calm_ms) &&
    isCount(tuning.gap_ms) &&
    isCount(tuning.sample_ms) &&
    isCount(tuning.share_permille) &&
    isCount(tuning.settle_ms)
  );
}

/** How an OCR area shows what it read. */
export type PanelOcr = 'in_place' | 'rendered';

/** An area's depth tier. */
export type PanelDepth = 'front' | 'auto' | 'back';

/** What a sticky area does when its window is resized. */
export type PanelOnResize = 'scale' | 'scale_position' | 'corner' | 'top_left';

/** The tiers, in the order the panel shows them (the menu's order). */
export const DEPTHS: readonly PanelDepth[] = ['front', 'auto', 'back'];

/** The answers to a resize, in the order the panel shows them. */
export const ON_RESIZE: readonly PanelOnResize[] = [
  'scale',
  'scale_position',
  'corner',
  'top_left',
];

/** The open area, as Rust's `PanelView` sends it. */
export interface PanelView {
  /** The area's id. */
  id: number;
  /** Its type's wire name. */
  kind: string;
  /** Its name, or `null`. */
  name: string | null;
  /** What a watch area tells the user about; `null` for another type. */
  watch: PanelWatch | null;
  /** Whether it is paused; `null` for a type with nothing to pause. */
  held: boolean | null;
  /** How a watch area reads and reports; `null` for another type. */
  tuning: PanelTuning | null;
  /** The types it can be turned into, by wire name. */
  kinds: string[];
  /** How an OCR area shows what it read; `null` for another type. */
  ocr: PanelOcr | null;
  /** Its depth tier. */
  depth: PanelDepth;
  /** Whether it follows the window under it. */
  sticky: boolean;
  /** Whether the stick switch can be used (a window to stick to, or stuck). */
  can_stick: boolean;
  /** What a sticky area does on a resize; `null` when it is free. */
  on_resize: PanelOnResize | null;
  /** Whether clicks pass through it. */
  click_through: boolean;
  /** Whether it holds a capture to copy or save. */
  capture: boolean;
}

/** The event that tells the page its area changed. */
export const PANEL_EVENT = 'panel://changed';

const oneOf = <T extends string>(
  allowed: readonly T[],
  value: unknown,
): value is T =>
  typeof value === 'string' && (allowed as readonly string[]).includes(value);

/** Whether a value from Rust is a panel view. */
export function isPanelView(value: unknown): value is PanelView {
  if (typeof value !== 'object' || value === null) return false;
  const view = value as Record<string, unknown>;
  return (
    typeof view.id === 'number' &&
    typeof view.kind === 'string' &&
    (view.name === null || typeof view.name === 'string') &&
    (view.watch === null || oneOf(['changes', 'settles'], view.watch)) &&
    (view.held === null || typeof view.held === 'boolean') &&
    (view.tuning === null || isPanelTuning(view.tuning)) &&
    Array.isArray(view.kinds) &&
    view.kinds.every((kind) => typeof kind === 'string') &&
    (view.ocr === null || oneOf(['in_place', 'rendered'], view.ocr)) &&
    oneOf(DEPTHS, view.depth) &&
    typeof view.sticky === 'boolean' &&
    typeof view.can_stick === 'boolean' &&
    (view.on_resize === null || oneOf(ON_RESIZE, view.on_resize)) &&
    typeof view.click_through === 'boolean' &&
    typeof view.capture === 'boolean'
  );
}
