/**
 * The overlay's side of the three-state interaction model (roadmap task 1.6,
 * ADR-0012). The Rust side owns the state machine and emits the current state
 * on `overlay://state`; this module holds the small, testable pieces the
 * component needs — the focus-indicator geometry and the escape intent — kept
 * out of the Svelte component so they need no DOM harness (as with `regions`).
 */

import type { CssRect, Invoke } from './regions';

/** Which of the three interaction states the overlay is in (ADR-0012). */
export type OverlayStateName = 'hidden' | 'placement' | 'living';

/**
 * A monitor's bounds in **physical virtual-desktop pixels**, as Rust sends them
 * — a `(x, y, width, height)` tuple, which serde encodes as a JSON array.
 */
export type PhysRect = [x: number, y: number, width: number, height: number];

/** The overlay's virtual-desktop origin (its inner top-left), physical px. */
export type Origin = [x: number, y: number];

/** The payload of the `overlay://state` event. */
export interface StatePayload {
  state: OverlayStateName;
  origin: Origin;
  monitors: PhysRect[];
}

/**
 * Converts each monitor's physical rect into a CSS rect in the overlay's
 * viewport, for drawing the per-monitor focus frame.
 *
 * The conversion uses **the WebView's own `devicePixelRatio`**, not a value
 * from Rust: the WebView is the authority on the scale it laid out in
 * (ADR-0011), and deriving it anywhere else reintroduces the scale-mismatch bug
 * that ADR exists to prevent. CSS `(0, 0)` is the overlay's top-left, which is
 * the virtual-desktop origin, so a monitor at physical `(mx, my)` sits at
 * `((mx − ox) / dpr, (my − oy) / dpr)`.
 *
 * Returns no frames for a non-finite or non-positive `dpr` rather than emitting
 * `NaN`-positioned rectangles — the same fail-safe posture the Rust scale check
 * takes. Better no indicator than a garbage one.
 */
export function monitorFramesCss(
  monitors: readonly PhysRect[],
  origin: Origin,
  dpr: number,
): CssRect[] {
  if (!Number.isFinite(dpr) || dpr <= 0) return [];
  const [ox, oy] = origin;
  return monitors.map(([x, y, width, height]) => ({
    x: (x - ox) / dpr,
    y: (y - oy) / dpr,
    width: width / dpr,
    height: height / dpr,
  }));
}

/** Whether this state dims the screen and shows the focus frames (Placement). */
export function showsTint(state: OverlayStateName): boolean {
  return state === 'placement';
}

/**
 * Emits the `Esc` intent. Never throws: `Esc` is a dismiss path, and an
 * unhandled rejection here would strand the user with the overlay holding
 * focus. Returns whether the intent landed.
 */
export async function escapeOverlay(invoke: Invoke): Promise<boolean> {
  try {
    await invoke('overlay_escape');
    return true;
  } catch (error) {
    console.error('Failed to emit the escape intent:', error);
    return false;
  }
}
