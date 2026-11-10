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
   * that must change before it counts; 0 is any change, 500 half the area. */
  share_permille: number;
}

/** The minimum gaps the panel offers, in ms: none, 0.1 s, 1 s, 10 s, 1 min. */
export const GAPS_MS: readonly number[] = [0, 100, 1_000, 10_000, 60_000];

/** The calm slider's steps, in ms, shortest first. */
export const CALM_STEPS_MS: readonly number[] = [
  50, 100, 150, 250, 500, 1_000, 2_000, 5_000, 10_000,
];

/** The sampling slider's steps, in ms between readings, slowest first so
 * the slider runs from every five seconds on the left to realtime (0) on
 * the right. */
export const SAMPLE_STEPS_MS: readonly number[] = [
  5_000, 4_000, 3_000, 2_000, 1_500, 1_000, 750, 500, 250, 100, 0,
];

/** The tolerance slider's steps, in thousandths of the area: any change
 * on the left, half the area on the right (his *"from any pixel to half the
 * area"*). */
export const SHARE_STEPS_PERMILLE: readonly number[] = [
  0, 1, 2, 5, 10, 20, 50, 100, 200, 300, 500,
];

/** The index of the step closest to `value`, for a slider showing a value
 * Rust kept that the steps may not hold exactly. */
export function nearestStep(steps: readonly number[], value: number): number {
  let best = 0;
  steps.forEach((step, index) => {
    if (Math.abs(step - value) < Math.abs((steps[best] ?? 0) - value)) {
      best = index;
    }
  });
  return best;
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
    isCount(tuning.share_permille)
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
