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
  /**
   * The type armed for the next drag, or `null` for none (ADR-0018 §3).
   * **Absence means `Default`** — the indicator shows no type cue when nothing
   * is armed, rather than naming the resting state as if it were a choice.
   */
  armed: ArmableType | null;
  /**
   * Whether the screen is frozen (task 1.9d, ADR-0026). Only true in placement.
   */
  frozen: boolean;
  /**
   * Each frozen still: its monitor rect in physical px, plus the URL its image
   * is served at. Empty whenever {@link frozen} is false.
   *
   * The URL ends `.png` regardless of what the still is actually encoded as —
   * an opaque versioned identifier, and since ADR-0027 the display format
   * defaults to JPEG. Nothing here may infer the format from the suffix.
   *
   * Rust derives both from one read, so a payload cannot claim frozen with no
   * stills — which would render as a live screen the app believes is frozen.
   */
  stills: FrozenStill[];
  /**
   * The `Ctrl+Space` → painted probe, or `null`.
   *
   * Present on exactly one payload per freeze — the one carrying the new
   * stills — and always `null` in a release build, where Rust stamps none.
   * Echo it with {@link reportFreezeLatency}, never with
   * {@link reportLatency}: the two measure different rows.
   */
  freeze_probe: number | null;
}

/** One monitor's frozen still: where it is, and where to fetch it. */
export interface FrozenStill {
  rect: PhysRect;
  url: string;
}

/**
 * Turns the wire form of the stills — `[x, y, w, h, url]` tuples, which is what
 * serde produces for a Rust tuple — into rects the layout helpers accept.
 *
 * Kept as a tuple on the wire rather than a struct because the monitor list
 * beside it already travels that way; converting in one place here is cheaper
 * than a second convention.
 */
export function stillsFromWire(
  wire: [number, number, number, number, string][],
): FrozenStill[] {
  return wire.map(([x, y, width, height, url]) => ({
    rect: [x, y, width, height],
    url,
  }));
}

/** An area's stacking tier (ADR-0013). */
export type LayerName = 'front' | 'auto' | 'back';

/**
 * Every area type on the wire, matching `type_name` in `overlay.rs`.
 *
 * All seven are listed because Rust sends all seven, not only the ones a
 * gesture can create. {@link ArmableType} is the narrower set a key can arm.
 *
 * This union is still hand-written, and it is no longer unchecked. UP-TAKE
 * `I-55` was that an eighth `AreaType` forces an arm in `type_name`'s exhaustive
 * match and leaves this file silent, so the new name would arrive at runtime
 * outside the union with no type error and no failing test, and the area would
 * draw as a default one. `area-kinds.test.ts` reads `type_name` out of the Rust
 * source and compares the two sets, so that change now goes red here as well.
 *
 * ⚠️ **The check is a text comparison, not a generated file.** It fails loudly
 * if `type_name` is renamed or its `match` reshaped, rather than passing on an
 * empty extraction, but it cannot see a name that reaches the frontend by any
 * route other than that function.
 */
export type AreaKind =
  | 'default'
  | 'screenshot'
  | 'record'
  | 'ocr'
  | 'upscale'
  | 'analysis'
  | 'filter';

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
  kind: AreaKind;
  /**
   * Magnification (§3.4), `1` at natural size.
   *
   * **Nothing here scales by it.** The magnified pixels arrive as a pin sized
   * to fill the area, so the ratio is already in the image; this is what lets
   * the area *say* how far it is zoomed, which is what makes scrolling back
   * discoverable.
   */
  zoom: number;
}

/** The payload of the `overlay://areas` event: every area, bottom-first. */
export interface AreasPayload {
  areas: AreaView[];
}

/**
 * The payload of `overlay://flash`: a user-initiated action on this area
 * succeeded, and the area should acknowledge it.
 *
 * `nonce` changes on every emit so a repeat of the same action on the same area
 * restarts the animation instead of being coalesced into no visible change —
 * which would fail in exactly the way this feature exists to fix.
 */
export interface FlashPayload {
  id: number;
  nonce: number;
}

/**
 * The payload of `overlay://active-monitor`: which monitor holds the cursor.
 *
 * An index into the `monitors` array of the last {@link StatePayload}. `null`
 * when the cursor is in a dead zone between mismatched monitors.
 */
export interface ActiveMonitorPayload {
  index: number | null;
}

/**
 * The payload of the `overlay://pin` event: one area's capture is ready.
 *
 * Carries a **URL, not bytes**. The `uptake-area://` scheme exists precisely so
 * a ~270 KB capture never crosses this JSON bridge — see the Rust `captures`
 * module.
 */
export interface PinPayload {
  id: number;
  /**
   * `null` means this area's pixels are **gone** and it must draw the live
   * screen again. Two cases produce it: a scroll back to natural size (§3.4's
   * floor), and a type conversion away from **any magnified area** or from a
   * `Screenshot` whose pin was its whole content (roadmap 1.27). Before zoom
   * existed a pin was only ever dropped along with its area, so this event never
   * had to say so.
   *
   * The second case said "a magnified `Default`" until 2026-08-22, when roadmap
   * 1.24 made it under-describe its own set: every `Upscale` area is magnified
   * by construction, so `Upscale` to `Screenshot` and `Upscale` to `Filter` --
   * both offered in the menu -- reach it too. `set_kind`'s condition is
   * `was_magnified && !supports_zoom(new)`, which never named a type.
   */
  url: string | null;
}

/**
 * The payload of the `overlay://hover` event: the area under the cursor.
 *
 * **It carries no cursor**, and that is settled rather than missing. This
 * interface briefly had a `cursor` keyword for the frontend to apply; it never
 * did anything, because the overlay is `WS_EX_TRANSPARENT` in every visible
 * state (ADR-0016) and so receives no `WM_SETCURSOR` at any position. ADR-0025
 * chose the surviving route — a narrow `SetSystemCursor` override on the Rust
 * side — and deleted this half rather than leave it looking functional.
 */
export interface HoverPayload {
  id: number | null;
  /**
   * Reveal the hovered area's close control without lighting the area up.
   *
   * Set when the cursor is merely inside a pass-through area rather than on
   * something it would grab. The highlight means *this is what a press will
   * take* (see the `.area.hovered` rule's own comment), which a pass-through
   * body does not honour, so one id cannot carry both facts. Rust decides it;
   * this side must not re-derive it, because the rule that produces it is the
   * Living input rule and that lives in one place on purpose.
   */
  chromeOnly: boolean;
}

/** One row of the per-area menu, positioned by Rust. */
export interface MenuItemView {
  rect: PhysRect;
  label: string;
  checked: boolean;
  /**
   * This row opens a child list, so it draws the marker that says so.
   *
   * One lowercase word, like every other key in this payload, and that is
   * deliberate rather than incidental: `UT-F-72` is a payload key that reached
   * this side only because of a `#[serde(rename_all)]` no gate in the repository
   * could see being removed. A name that is identical in both conventions needs
   * no attribute and so has nothing to lose.
   */
  parent: boolean;
}

/** The open child list of one parent row (roadmap 1.28). */
export interface ChildMenuView {
  rect: PhysRect;
  items: MenuItemView[];
  hovered: number | null;
  /**
   * Index into {@link MenuView.items} of the row this list belongs to, drawn as
   * open. Separate from `hovered` because both are true at once: while the
   * pointer is in this list nothing at the top level is hovered, and the row it
   * came from still has to read as its source.
   */
  owner: number;
}

/** The open per-area menu (ADR-0013's Layer control). */
export interface MenuView {
  rect: PhysRect;
  items: MenuItemView[];
  hovered: number | null;
  /** The open child list, drawn on top of the rows beneath it. */
  child: ChildMenuView | null;
}

/** The payload of the `overlay://menu` event; `menu` is null when none is open. */
export interface MenuPayload {
  menu: MenuView | null;
}

/**
 * A magnification as the badge prints it: `2×`, `1.25×`, `3.5×`.
 *
 * **Trailing zeros are dropped rather than padded to a fixed width.** The
 * factor moves in quarters, so a fixed two decimals prints `2.00×` for the
 * commonest value in the range and reads as a measurement rather than as a
 * setting. `toFixed(2)` first, then trim, because the value has crossed a JSON
 * bridge as an IEEE double and `1.25 * 3` is not exactly `3.75`.
 */
export function formatZoom(factor: number): string {
  const fixed = factor.toFixed(2).replace(/\.?0+$/, '');
  return `${fixed}×`;
}

/** An area ready to draw: CSS geometry plus the state that styles it. */
export interface AreaFrame {
  id: number;
  rect: CssRect;
  close: CssRect;
  layer: LayerName;
  kind: AreaKind;
  /** Magnification (§3.4), `1` at natural size. See {@link AreaView.zoom}. */
  zoom: number;
  /** Draw this area as the one a press would grab: brighter, lit border. */
  hovered: boolean;
  /** Draw this area's close control. A superset of {@link AreaFrame.hovered}:
   * every highlighted area shows its control, and a pass-through area the cursor
   * is inside shows the control alone. */
  showClose: boolean;
  /** This area is the source of a live move or resize: draw it as where the
   * area is coming *from*, not as a second area. */
  source: boolean;
}

/** One drawable row. */
export interface MenuItemFrame {
  rect: CssRect;
  label: string;
  checked: boolean;
  hovered: boolean;
  /** Draw the marker that says this row opens a child list. */
  parent: boolean;
  /** This row's child list is open, so draw it as the list's source. */
  open: boolean;
}

/** The open menu ready to draw. */
export interface MenuFrame {
  rect: CssRect;
  items: MenuItemFrame[];
  /** The open child list, or null when no parent row has one open. */
  child: { rect: CssRect; items: MenuItemFrame[] } | null;
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
  draggedId: number | null = null,
  hoverChromeOnly = false,
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
    kind: area.kind,
    zoom: area.zoom,
    // A dragged area is not also "hovered": the hover chrome invites a gesture
    // that is already under way, and its close control would sit at the source
    // position while the cursor is somewhere else entirely.
    //
    // `hoverChromeOnly` splits what used to be one flag. The highlight is a
    // claim about what a press will grab, so it is withheld when the hover came
    // from containment alone; the control is not a claim about anything and is
    // shown either way. Without the split a large Filter area sat permanently
    // lit with a permanent close control over the user's content.
    hovered: area.id === hoveredId && area.id !== draggedId && !hoverChromeOnly,
    showClose: area.id === hoveredId && area.id !== draggedId,
    source: area.id === draggedId,
  }));
}

/** One list's rows in CSS geometry, or `null` when the conversion fails. */
function menuItemsCss(
  view: { items: MenuItemView[]; hovered: number | null },
  origin: Origin,
  dpr: number,
  openIndex: number | null = null,
): MenuItemFrame[] | null {
  const rects = physRectsToCss(
    view.items.map((item) => item.rect),
    origin,
    dpr,
  );
  if (rects.length !== view.items.length) return null;
  return view.items.map((item, index) => ({
    rect: rects[index] as CssRect,
    label: item.label,
    checked: item.checked,
    hovered: index === view.hovered,
    parent: item.parent,
    open: index === openIndex,
  }));
}

/**
 * Converts the open menu into drawable geometry, or `null` when none is open.
 *
 * The child list's own `null` checks are **forced by the types and unreachable
 * in practice**, and saying so is the point. The only way a conversion fails is
 * an unusable `dpr` ({@link physRectsToCss}), `dpr` is one scalar shared by both
 * lists, and the parent is converted first, so there is no input for which the
 * parent succeeds and the child does not.
 *
 * An earlier version of this comment claimed the branch *"drops the whole menu,
 * rather than drawing the parent alone"*, and a test asserted it. Both were
 * describing behaviour no input can produce: an independent review mutated the
 * branch to draw the parent alone and the whole suite stayed green, because the
 * only case reaching it is the `dpr` case that returns two lines earlier. The
 * claim is gone and the test with it; the `null` handling stays because
 * TypeScript requires it, which is a different and honest reason.
 */
export function menuFrameCss(
  menu: MenuView | null,
  origin: Origin,
  dpr: number,
): MenuFrame | null {
  if (menu === null) return null;
  const rect = physRectToCss(menu.rect, origin, dpr);
  if (rect === null) return null;
  const items = menuItemsCss(menu, origin, dpr, menu.child?.owner ?? null);
  if (items === null) return null;
  let child: MenuFrame['child'] = null;
  if (menu.child !== null) {
    const childRect = physRectToCss(menu.child.rect, origin, dpr);
    const childItems = menuItemsCss(menu.child, origin, dpr);
    if (childRect === null || childItems === null) return null;
    child = { rect: childRect, items: childItems };
  }
  return { rect, items, child };
}

/**
 * The payload of the `placement://selection` event: the live drag rectangle, or
 * `null` when nothing is being dragged.
 */
export interface SelectionPayload {
  rect: PhysRect | null;
  /**
   * A latency probe on sampled frames, or null. Echo it back **after this frame
   * has painted** and Rust closes the loop on its own clock — the value is
   * opaque here on purpose, so no epoch has to be reconciled across the bridge.
   */
  probe: number | null;
  /**
   * The area being moved or resized, if any. It is drawn as the drag's *source*
   * — where the area is coming from — rather than as a normal area, so a move
   * does not look like two areas existing at once.
   */
  source: number | null;
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

/**
 * A CSS rect reduced to a comparison key.
 *
 * The `{#each}` over the monitor frames already keys on exactly this, so the
 * two cannot disagree about what "the same monitor" means.
 */
export function frameKey(frame: CssRect): string {
  return `${frame.x},${frame.y},${frame.width},${frame.height}`;
}

/**
 * The frames that are showing a frozen still, as {@link frameKey}s.
 *
 * # Why this exists, and why a boolean was wrong
 *
 * Until 2026-08-05 the component asked one question — *are there any stills?* —
 * and painted the `frozen` badge on **every** monitor frame if the answer was
 * yes. That was correct for exactly as long as a freeze covered the whole
 * desktop. [ADR-0026]'s third amendment narrows a freeze to the cursor's
 * monitor, so on a four-monitor desktop one still would have put the word
 * *frozen* on three live screens.
 *
 * **That is the inverse of the thing the amendment was for.** Its *Honesty at
 * the boundary* argument is that the un-frozen monitors visibly stay live, so
 * the user can see what they will get rather than being told something false.
 * A badge derived from a count told them something false. Found in the
 * independent review of PR #42, before the narrowing shipped.
 *
 * Derived from the **converted** frames rather than from the raw stills, which
 * preserves a property the conversion's own comment relies on: a still whose
 * rect will not convert is dropped rather than drawn at a fallback position,
 * and that monitor must then read as live, because it is.
 *
 * [ADR-0026]: the private planning repo's
 * `DECISIONS/ADR-0026-freeze-on-demand-trigger.md`
 */
export function frozenFrameKeys(
  stillFrames: readonly { frame: CssRect }[],
): Set<string> {
  return new Set(stillFrames.map((still) => frameKey(still.frame)));
}

/** Whether this state dims the screen and shows the focus frames (Placement). */
export function showsTint(state: OverlayStateName): boolean {
  return state === 'placement';
}

/**
 * Whether this state may show the per-area menu — any visible state. The menu
 * is reachable from Living too (ADR-0016: right-click on the topmost
 * interactive area), not just from Placement; only a hidden overlay can never
 * legitimately have one, and Rust closes the menu on every transition to
 * hidden anyway, so this is the belt to that suspender.
 */
export function showsMenu(state: OverlayStateName): boolean {
  return state !== 'hidden';
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
 * Whether this event is the freeze toggle — `Ctrl+Space` (ADR-0026).
 *
 * # Why a chord where arming uses a bare letter
 *
 * Bare single letters are the *arming* namespace: `s` is Screenshot, `f` is
 * Filter and `u` is Upscale today, and OCR, Analysis and Record each want their
 * initial. A view toggle taking one would spend a slot the type system needs.
 * `armedTypeForKey` below rejects every `Ctrl`/`Alt`/`Meta` chord by
 * construction, so this chord **cannot** collide with arming however many types
 * are added.
 *
 * This listed Filter and Upscale among the types that merely *want* an initial
 * until 2026-08-21, 54 lines above the arms that give them one. Roadmap 1.23
 * spent `f` and 1.24 spent `u`; three of the seven are claimed, not one.
 *
 * `Space` alone was the better key on ergonomics and was deliberately left
 * unclaimed for a future area-level action (ADR-0026 decision 8).
 *
 * `Alt` is excluded because it already means "suppress snapping" during
 * placement; `Meta` because `Win+Space` is the Windows layout switcher.
 */
export function isFreezeKey(
  event: Pick<KeyboardEvent, 'key' | 'ctrlKey' | 'altKey' | 'metaKey'>,
): boolean {
  // `event.key` for the space bar is a single space, not `'Space'` — that is
  // `event.code`. Testing the wrong one is a binding that never fires and looks
  // like a dead feature.
  return event.key === ' ' && event.ctrlKey && !event.altKey && !event.metaKey;
}

/**
 * An area type a direct key can arm for the next drag (ADR-0018 §1).
 *
 * Kept in step with `armable_type` in `overlay.rs` by hand, because the wire
 * has no shared schema. Rust rejects a name it does not know, so a drift here
 * fails as a refused arm rather than as an area of the wrong type.
 */
export type ArmableType = Extract<
  AreaKind,
  'screenshot' | 'filter' | 'upscale'
>;

/**
 * Which type this key arms, or `null` if it arms nothing.
 *
 * Takes the event rather than the bare key, unlike {@link isRemoveKey} — and
 * the difference is deliberate. Arming changes what the *next gesture means*,
 * so a chord the user pressed for some other reason must not trigger it, and
 * `Alt` in particular already has a meaning during placement (it suppresses
 * snapping). A guard that lives in the predicate cannot be forgotten at a call
 * site; one that lives in the handler can.
 *
 * `Shift` is not excluded: `Shift+S` is just how a capital `S` is typed.
 */
export function armedTypeForKey(
  event: Pick<KeyboardEvent, 'key' | 'ctrlKey' | 'altKey' | 'metaKey'>,
): ArmableType | null {
  if (event.ctrlKey || event.altKey || event.metaKey) {
    return null;
  }
  switch (event.key.toLowerCase()) {
    case 's':
      return 'screenshot';
    case 'f':
      return 'filter';
    // Roadmap 1.24. `U` for Upscale; the area is born magnified on the Rust
    // side, so this arms it and nothing here knows the factor.
    case 'u':
      return 'upscale';
    default:
      return null;
  }
}

/**
 * Arms the type of the next drag. Never throws, for the same reason
 * {@link escapeOverlay} does not.
 *
 * Rust rejects arming outside placement, and that rejection is **expected**
 * rather than exceptional — the key handler is live in `living` too. Logging it
 * as an error would train the reader to ignore the console.
 */
export async function armAreaType(
  invoke: Invoke,
  kind: ArmableType,
): Promise<boolean> {
  try {
    await invoke('overlay_arm_type', { kind });
    return true;
  } catch {
    return false;
  }
}

/**
 * Toggles the frozen view (task 1.9d, ADR-0026). Never throws, for the same
 * reason {@link armAreaType} does not.
 *
 * Rust decides whether the toggle applies — it is Placement-only — so this
 * sends the intent unconditionally rather than gating on a state the frontend
 * holds a copy of. Two places deciding one thing is how the stale-menu defect
 * of 1.6c happened.
 */
export async function toggleFreeze(invoke: Invoke): Promise<boolean> {
  try {
    await invoke('overlay_toggle_freeze');
    return true;
  } catch {
    return false;
  }
}

/**
 * Returns a latency probe once the frame carrying it has actually painted.
 *
 * **Two nested `requestAnimationFrame` calls, not one.** A rAF callback runs
 * *before* the paint it belongs to, so measuring there would stop the clock
 * early and flatter the result. The second callback runs on the following
 * frame, by which point the first has been painted — the standard way to
 * observe "after paint" from script.
 *
 * Never throws: this is instrumentation, and a failed measurement must not
 * disturb the thing it measures.
 */
export function reportLatency(invoke: Invoke, probe: number): void {
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      void invoke('overlay_report_latency', { probe }).catch(() => {});
    });
  });
}

/**
 * Returns the freeze probe once every still has **decoded** and the following
 * frame has painted — `quality-bars.md` §1's *`Ctrl+Space` → frozen view
 * painted* row.
 *
 * **Why this is not {@link reportLatency}.** A double `requestAnimationFrame`
 * resolves as soon as the DOM has updated, and for freeze the DOM updates the
 * instant the `<img>` elements are inserted — while four full-monitor stills
 * are still decoding. Decode is precisely the cost this row exists to capture, and
 * 1.9f's measured `72–78 ms` stops at the encode, so timing the rAF pair alone
 * would report a comfortable number excluding the only unmeasured stage. That
 * is `UT-F-41`: an instrument whose summary says the opposite of the thing it
 * was built to settle.
 *
 * So `decode()` is awaited on every image first, and the clock stops after
 * *decoded and painted* rather than after *inserted*.
 *
 * A rejected `decode()` — a still whose image 404s — counts as decoded rather
 * than aborting the measurement. The alternative is silence, and a probe that
 * reports nothing when something is wrong is indistinguishable from one that is
 * switched off (`I-11`). A broken still instead shows as an implausibly fast
 * figure beside a visibly broken screen, which is readable.
 *
 * Never throws: instrumentation must not disturb what it measures.
 */
export async function reportFreezeLatency(
  invoke: Invoke,
  probe: number,
  images: HTMLImageElement[],
): Promise<void> {
  await Promise.all(images.map((image) => image.decode().catch(() => {})));
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      void invoke('overlay_report_freeze_latency', { probe }).catch(() => {});
    });
  });
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
