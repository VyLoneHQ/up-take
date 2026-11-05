/**
 * The area settings panel (roadmap 1.60), the page side of
 * `src-tauri/src/panel.rs`. Rust opens, places and closes the panel's window
 * beside its area; the page reads the area with `panel_view` whenever
 * `panel://changed` arrives and sets it through the `panel_set_*` commands.
 */

/** What a watch area tells the user about. */
export type PanelWatch = 'changes' | 'settles';

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
