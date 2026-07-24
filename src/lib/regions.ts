/**
 * Shared frontend primitives (originally task 1.2's interactive-region
 * reporting module).
 *
 * The region reporting itself — `measure` + `reportInteractiveRegions` — was
 * deleted with ADR-0016: the overlay window is never interactive, so there are
 * no frontend-measured regions for Rust to hit-test any more, and per-area
 * input goes through the Rust-side mouse hook against Rust-owned rectangles.
 * What survives is the CSS-rect type every conversion in `overlay-state.ts`
 * uses, and the dismiss-key predicate.
 *
 * Kept out of the Svelte component so it can be tested without a DOM harness,
 * and so the component stays what architecture §1 asks for: render state, emit
 * intents, hold no logic.
 */

/** A rectangle in CSS pixels, relative to the overlay's viewport. */
export interface CssRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The Tauri `invoke` function, injected so tests need no module mocking. */
export type Invoke = (
  command: string,
  args?: Record<string, unknown>,
) => Promise<unknown>;

/** Whether a key press should dismiss the overlay (M-11 keyboard-only). */
export function isDismissKey(key: string): boolean {
  return key === 'Escape';
}
