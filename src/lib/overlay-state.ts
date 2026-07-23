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

/** An area's stacking tier (ADR-0013). */
export type LayerName = 'front' | 'auto' | 'back';

/**
 * One area as Rust sends it. Every rectangle is **physical** and already laid
 * out by Rust, including the close control's: the overlay is click-through, so
 * that control is hit-tested against the Rust-side rectangle rather than as a
 * DOM element, and computing a second one here is how it would end up drawn
 * somewhere it cannot be clicked.
 */
export interface AreaView {
  id: number;
  rect: PhysRect;
  close: PhysRect;
  layer: LayerName;
}

/** The payload of the `overlay://areas` event: every area, bottom-first. */
export interface AreasPayload {
  areas: AreaView[];
}

/** The payload of the `overlay://hover` event: the area under the cursor. */
export interface HoverPayload {
  id: number | null;
}

/** One row of the per-area menu, positioned by Rust. */
export interface MenuItemView {
  rect: PhysRect;
  label: string;
  checked: boolean;
}

/** The open per-area menu (ADR-0013's Layer control). */
export interface MenuView {
  rect: PhysRect;
  items: MenuItemView[];
  hovered: number | null;
}

/** The payload of the `overlay://menu` event; `menu` is null when none is open. */
export interface MenuPayload {
  menu: MenuView | null;
}

/** An area ready to draw: CSS geometry plus the state that styles it. */
export interface AreaFrame {
  id: number;
  rect: CssRect;
  close: CssRect;
  layer: LayerName;
  hovered: boolean;
}

/** The open menu ready to draw. */
export interface MenuFrame {
  rect: CssRect;
  items: { rect: CssRect; label: string; checked: boolean; hovered: boolean }[];
}

/**
 * Converts the area set into drawable frames, marking the hovered one.
 *
 * Returns nothing when the `dpr` is unusable, matching {@link physRectsToCss} —
 * an area drawn at a `NaN` position is worse than an area not drawn, because it
 * still cannot be clicked but now also hides what is underneath.
 */
export function areaFramesCss(
  areas: readonly AreaView[],
  origin: Origin,
  dpr: number,
  hoveredId: number | null,
): AreaFrame[] {
  const rects = physRectsToCss(
    areas.map((area) => area.rect),
    origin,
    dpr,
  );
  const closes = physRectsToCss(
    areas.map((area) => area.close),
    origin,
    dpr,
  );
  if (rects.length !== areas.length || closes.length !== areas.length)
    return [];
  return areas.map((area, index) => ({
    id: area.id,
    // Checked above, so these are present; the non-null assertions keep the
    // types honest without a second guard per element.
    rect: rects[index] as CssRect,
    close: closes[index] as CssRect,
    layer: area.layer,
    hovered: area.id === hoveredId,
  }));
}

/** Converts the open menu into drawable geometry, or `null` when none is open. */
export function menuFrameCss(
  menu: MenuView | null,
  origin: Origin,
  dpr: number,
): MenuFrame | null {
  if (menu === null) return null;
  const rect = physRectToCss(menu.rect, origin, dpr);
  if (rect === null) return null;
  const items = physRectsToCss(
    menu.items.map((item) => item.rect),
    origin,
    dpr,
  );
  if (items.length !== menu.items.length) return null;
  return {
    rect,
    items: menu.items.map((item, index) => ({
      rect: items[index] as CssRect,
      label: item.label,
      checked: item.checked,
      hovered: index === menu.hovered,
    })),
  };
}

/**
 * The payload of the `placement://selection` event: the live drag rectangle, or
 * `null` when nothing is being dragged.
 */
export interface SelectionPayload {
  rect: PhysRect | null;
}

/**
 * Converts physical virtual-desktop rects into CSS rects in the overlay's
 * viewport. The one conversion shared by every physical rect the overlay draws
 * — the per-monitor focus frames, the persistent area borders, and the live
 * placement selection box.
 *
 * It uses **the WebView's own `devicePixelRatio`**, not a value from Rust: the
 * WebView is the authority on the scale it laid out in (ADR-0011), and deriving
 * it anywhere else reintroduces the scale-mismatch bug that ADR exists to
 * prevent. CSS `(0, 0)` is the overlay's top-left, which is the virtual-desktop
 * origin, so a rect at physical `(x, y)` sits at `((x − ox) / dpr, (y − oy) / dpr)`.
 *
 * Returns nothing for a non-finite or non-positive `dpr` rather than emitting
 * `NaN`-positioned rectangles — the same fail-safe posture the Rust scale check
 * takes. Better nothing drawn than a garbage rectangle.
 */
export function physRectsToCss(
  rects: readonly PhysRect[],
  origin: Origin,
  dpr: number,
): CssRect[] {
  if (!Number.isFinite(dpr) || dpr <= 0) return [];
  const [ox, oy] = origin;
  return rects.map(([x, y, width, height]) => ({
    x: (x - ox) / dpr,
    y: (y - oy) / dpr,
    width: width / dpr,
    height: height / dpr,
  }));
}

/**
 * Converts the monitor rects into the per-monitor focus frames (Placement).
 * A thin wrapper over {@link physRectsToCss} kept for the component's clarity.
 */
export function monitorFramesCss(
  monitors: readonly PhysRect[],
  origin: Origin,
  dpr: number,
): CssRect[] {
  return physRectsToCss(monitors, origin, dpr);
}

/**
 * Converts a single physical rect (the live selection box) into a CSS rect, or
 * `null` when there is nothing to draw or the `dpr` is unusable.
 */
export function physRectToCss(
  rect: PhysRect | null,
  origin: Origin,
  dpr: number,
): CssRect | null {
  if (rect === null) return null;
  return physRectsToCss([rect], origin, dpr)[0] ?? null;
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

/**
 * Whether this key removes the area under the cursor (PRODUCT-VISION §4.3:
 * `Delete` removes, `Esc` never does).
 *
 * `Backspace` is deliberately **not** included. On a keyboard without a
 * dedicated `Delete` key it is the obvious substitute, but it is also the key
 * people press reflexively to undo their last input — and dismissing an area
 * has no undo.
 */
export function isRemoveKey(key: string): boolean {
  return key === 'Delete';
}

/**
 * Removes the area under the cursor. Never throws, for the same reason
 * {@link escapeOverlay} does not: an unhandled rejection in a key handler is a
 * silent failure the user reads as the overlay having hung.
 */
export async function dismissFocusedArea(invoke: Invoke): Promise<boolean> {
  try {
    await invoke('overlay_dismiss_focused');
    return true;
  } catch (error) {
    console.error('Failed to dismiss the focused area:', error);
    return false;
  }
}
