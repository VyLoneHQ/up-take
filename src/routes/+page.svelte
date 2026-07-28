<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { onMount } from 'svelte';
import { SvelteMap } from 'svelte/reactivity';
import {
  type ActiveMonitorPayload,
  type AreaFrame,
  type AreasPayload,
  type AreaView,
  type ArmableType,
  areaFramesCss,
  armAreaType,
  armedTypeForKey,
  dismissFocusedArea,
  escapeOverlay,
  type FlashPayload,
  type HoverPayload,
  isRemoveKey,
  type MenuFrame,
  type MenuPayload,
  type MenuView,
  menuFrameCss,
  monitorFramesCss,
  type Origin,
  type OverlayStateName,
  type PhysRect,
  type PinPayload,
  physRectToCss,
  reportLatency,
  type SelectionPayload,
  type StatePayload,
  showsMenu,
  showsTint,
} from '$lib/overlay-state';
import { type CssRect, isDismissKey } from '$lib/regions';

// Presentation only (architecture §1): the Rust side owns the state machine
// (ADR-0012), the placement input (ADR-0014) and the area store; this component
// renders the focus indicator, the persistent area borders and the live
// selection box from the physical geometry Rust emits, and emits the Esc intent.
// No decision is made here.
//
// Everything is stored as the physical rects Rust sends, plus the current origin
// and `devicePixelRatio`, and converted to CSS reactively — so a display change
// (which re-emits the state with a new origin) re-lays-out the areas and frames
// without them each needing their own re-report.
let overlayState = $state<OverlayStateName>('hidden');
let monitors: PhysRect[] = $state([]);
let origin: Origin = $state([0, 0]);
let areas: AreaView[] = $state([]);
let selection: PhysRect | null = $state(null);
let draggedArea: number | null = $state(null);
let hoveredArea: number | null = $state(null);
let menu: MenuView | null = $state(null);
// What the next drag will make, or null for Default (ADR-0018 §3). Rust owns
// this — arming is placement state living beside the mouse hook — and re-emits
// the state whenever it changes.
let armed: ArmableType | null = $state(null);
// Which monitor holds the cursor — an index into `monitors`, both from Rust's
// one cached list. Null in a dead zone between mismatched monitors, in which
// case no badge is drawn at all rather than one guessed onto a screen.
let activeMonitor: number | null = $state(null);
// Each area's pinned capture URL, keyed by area id. Versioned URLs (see the
// Rust `captures` module), so a re-capture replaces the entry with a distinct
// address rather than relying on the WebView to bust its own cache.
let pins = $state(new SvelteMap<number, string>());
// Areas that just completed a Copy or Save, mapped to the nonce of that
// completion. Keyed rendering on the nonce is what restarts the animation when
// the same action runs twice on the same area.
let flashes = $state(new SvelteMap<number, number>());
// The WebView owns its scale (ADR-0011); refreshed on every state event in case
// the overlay moved to a monitor at a different DPI.
let dpr = $state(1);

const frames: CssRect[] = $derived(monitorFramesCss(monitors, origin, dpr));
// Hover chrome — the close control, the brighter border — shows in every
// visible state as of task 1.17(a).
//
// It was Placement-only, for a reason that had already stopped being true:
// "in Living the overlay does not own the pointer, so a control that appeared
// to follow the cursor would be one no click could reach." ADR-0016 gave Living
// per-area input via the global hook, and 1.17(a) lets a press there begin a
// real move or resize — so the pointer *is* owned over an interactive area, and
// a handle the user cannot see is a handle they will not reach for.
//
// Rust decides what counts as hovered in each state (interactive areas only in
// Living), so this no longer gates on the state at all — gating in both places
// is how the two would drift apart.
const areaFrames: AreaFrame[] = $derived(
  areaFramesCss(areas, origin, dpr, hoveredArea, draggedArea),
);
// The drag preview renders in every visible state as of task 1.17(a), because
// Living now has move and resize gestures of its own.
//
// It was gated on `placement`, which had been harmless while Living had no
// gestures — and became a bug the moment it did: `areaFrames` marks the dragged
// area as the *source* and stops drawing it (the preview IS the area for the
// duration), so a Living drag hid the area with nothing put in its place. The
// area appeared to vanish until the gesture ended. Two gates on one fact, and
// only one of them moved.
const selectionFrame: CssRect | null = $derived(
  overlayState === 'hidden' ? null : physRectToCss(selection, origin, dpr),
);
// The menu renders in every visible state, not just Placement: in Living it is
// opened by a right-click on an interactive area (ADR-0016).
const menuFrame: MenuFrame | null = $derived(
  showsMenu(overlayState) ? menuFrameCss(menu, origin, dpr) : null,
);

function onKeydown(event: KeyboardEvent) {
  if (isDismissKey(event.key)) {
    void escapeOverlay(invoke);
    return;
  }
  if (isRemoveKey(event.key)) {
    // `preventDefault` so the key cannot also reach anything else the WebView
    // might do with it; the overlay renders no editable content today, and this
    // keeps that true if it ever does.
    event.preventDefault();
    void dismissFocusedArea(invoke);
    return;
  }
  // A direct key arms the type of the next drag (ADR-0018 §1). Rust owns
  // whether that is legal in the current state, so this fires the intent and
  // lets it decide — the same division as every other key here.
  const arming = armedTypeForKey(event);
  if (arming) {
    event.preventDefault();
    void armAreaType(invoke, arming);
  }
}

onMount(() => {
  const unlistenState = listen<StatePayload>('overlay://state', (event) => {
    overlayState = event.payload.state;
    monitors = event.payload.monitors;
    origin = event.payload.origin;
    armed = event.payload.armed;
    dpr = window.devicePixelRatio;
    // A hidden overlay is drawing nothing; drop any half-finished selection so
    // it cannot reappear on the next show before the poll clears it.
    if (overlayState === 'hidden') {
      selection = null;
      draggedArea = null;
    }
  });
  const unlistenAreas = listen<AreasPayload>('overlay://areas', (event) => {
    areas = event.payload.areas;
    // Drop pins whose area is gone. Rust frees the bytes on dismiss; this is
    // the view's side of the same lifetime, and without it the map would keep
    // growing across a long session with URLs that now 404.
    const live = new Set(event.payload.areas.map((area) => area.id));
    for (const id of pins.keys()) {
      if (!live.has(id)) pins.delete(id);
    }
    for (const id of flashes.keys()) {
      if (!live.has(id)) flashes.delete(id);
    }
  });
  const unlistenActiveMonitor = listen<ActiveMonitorPayload>(
    'overlay://active-monitor',
    (event) => {
      activeMonitor = event.payload.index;
    },
  );
  const unlistenFlash = listen<FlashPayload>('overlay://flash', (event) => {
    flashes.set(event.payload.id, event.payload.nonce);
  });
  const unlistenPin = listen<PinPayload>('overlay://pin', (event) => {
    // Arrives ~200 ms after the area itself: the area appears the instant the
    // drag ends and fills in when its capture lands, rather than leaving a hole
    // where the user just dragged.
    pins.set(event.payload.id, event.payload.url);
  });
  const unlistenSelection = listen<SelectionPayload>(
    'placement://selection',
    (event) => {
      selection = event.payload.rect;
      draggedArea = event.payload.source;
      // Close the latency loop on sampled frames. Scheduled after this
      // assignment so the measurement covers the render it caused.
      if (event.payload.probe !== null) {
        reportLatency(invoke, event.payload.probe);
      }
    },
  );
  const unlistenHover = listen<HoverPayload>('overlay://hover', (event) => {
    hoveredArea = event.payload.id;
  });
  const unlistenMenu = listen<MenuPayload>('overlay://menu', (event) => {
    menu = event.payload.menu;
  });

  // Request the current state only *after* the listeners are registered.
  // `listen` resolves once the backend has recorded the subscription; requesting
  // before that races the reply and drops it — which is exactly the startup
  // case, where the overlay is already in Placement (with areas possibly already
  // present) when the webview mounts. Chaining on the promise closes the gap.
  const ready = Promise.all([
    unlistenState,
    unlistenAreas,
    unlistenActiveMonitor,
    unlistenFlash,
    unlistenPin,
    unlistenSelection,
    unlistenHover,
    unlistenMenu,
  ]);
  void ready.then(() => invoke('overlay_request_state'));
  return () => {
    void ready.then((unlisteners) => {
      for (const unlisten of unlisteners) unlisten();
    });
  };
});
</script>

<svelte:window onkeydown={onKeydown} />

<!-- No `cursor` style here, deliberately. This element carried one until
     ADR-0025: a click-through window receives no `WM_SETCURSOR`, so a CSS cursor
     on the overlay never applied at any position. Cursor feedback is a narrow
     `SetSystemCursor` override on the Rust side instead. -->
<main class="overlay" class:active={showsTint(overlayState)}>
  {#if showsTint(overlayState)}
    {#each frames as frame, i (`${frame.x},${frame.y},${frame.width},${frame.height}`)}
      <div
        class="monitor-frame"
        style="left: {frame.x}px; top: {frame.y}px; width: {frame.width}px; height: {frame.height}px"
      >
        <!-- What the next drag will make (ADR-0018 §3). Nothing is shown when
             nothing is armed: absence means Default, and a permanent label
             naming the resting state is the "which mode am I in?" noise the
             design avoids. This indicator is the thing that buys down the cost
             of having transient mode state at all, so it is deliberately loud.

             On the cursor's monitor ONLY (F-13). Shown on all of them at once —
             as the first cut did — it reads as "every screen is armed" and
             buries the single fact it exists to convey. -->
        {#if armed && i === activeMonitor}
          <span class="armed-badge">{armed}</span>
        {/if}
      </div>
    {/each}
  {/if}

  {#if overlayState !== 'hidden'}
    {#each areaFrames as area (area.id)}
      {#if !area.source}
        <div
          class="area"
          class:hovered={area.hovered}
          class:pinned={area.layer !== 'auto'}
          style="transform: translate3d({area.rect.x}px, {area.rect.y}px, 0); width: {area
            .rect.width}px; height: {area.rect.height}px"
        >
          {#if pins.get(area.id)}
            <!-- The Snipaste pin (ADR-0014 §6). The bytes arrive over the
                 uptake-area:// scheme, not the IPC bridge — see `captures`.
                 `draggable={false}` because the overlay owns the mouse in
                 placement and a native image drag would fight the hook. -->
            <img
              class="pin"
              src={pins.get(area.id)}
              alt=""
              draggable={false}
            />
          {/if}
          {#if flashes.has(area.id)}
            <!-- Acknowledges a completed Copy or Save. `{#key}` on the nonce
                 recreates the element, which is what restarts the animation
                 when the same action runs twice — otherwise the second Copy
                 would be as silent as no Copy at all.

                 `onanimationend` drops the entry, and that is a fix rather than
                 tidiness. A flash is a one-shot *event*, but `flashes` stored it
                 as durable state that nothing ever removed — so the entry
                 outlived its meaning and any remount of this div replayed the
                 animation. The div remounts on every drag, because `{#if
                 !area.source}` above stops rendering an area while it is being
                 dragged (the preview is the area for the duration). Net effect,
                 reported from the rig 2026-07-27: once an area had been copied,
                 finishing *any* later move or resize of it fired the Copy flash
                 again, claiming an export that never happened.

                 Deleting on animation end means the entry exists for exactly as
                 long as the animation it drives, so there is nothing left to
                 replay. A remount *during* those 420 ms legitimately continues
                 the acknowledgement. -->
            {#key flashes.get(area.id)}
              <span
                class="flash"
                onanimationend={() => flashes.delete(area.id)}
              ></span>
            {/key}
          {/if}
          {#if area.layer !== 'auto'}
            <span class="layer-badge">{area.layer === 'front' ? '▲' : '▼'}</span>
          {/if}
        </div>
      {/if}
      {#if area.hovered}
        <div
          class="close"
          style="transform: translate3d({area.close.x}px, {area.close.y}px, 0); width: {area
            .close.width}px; height: {area.close.height}px"
        >
          ×
        </div>
      {/if}
    {/each}
  {/if}

  {#if selectionFrame}
    <div
      class="selection"
      style="transform: translate3d({selectionFrame.x}px, {selectionFrame.y}px, 0); width: {selectionFrame.width}px; height: {selectionFrame.height}px"
    ></div>
  {/if}

  {#if menuFrame}
    <div
      class="menu"
      style="left: {menuFrame.rect.x}px; top: {menuFrame.rect.y}px; width: {menuFrame
        .rect.width}px; height: {menuFrame.rect.height}px"
    ></div>
    {#each menuFrame.items as item (item.label)}
      <div
        class="menu-item"
        class:hovered={item.hovered}
        style="left: {item.rect.x}px; top: {item.rect.y}px; width: {item.rect
          .width}px; height: {item.rect.height}px"
      >
        <span class="tick">{item.checked ? '✓' : ''}</span>
        <span class="label">{item.label}</span>
      </div>
    {/each}
  {/if}
</main>

<style>
:global(html),
:global(body) {
  margin: 0;
  padding: 0;
  background: transparent;
  overflow: hidden;
}

.overlay {
  position: fixed;
  inset: 0;
  user-select: none;
  cursor: default;
}

/* PLACEMENT: UP-TAKE has input focus (ADR-0012), delivered by the global mouse
   hook (ADR-0014) rather than by an interactive window — the overlay stays
   click-through so live content underneath is never degraded. No full-surface
   fill: a flat tint over a hardware video plane punches it to solid grey, and
   placing an area over live content is the core use case. The dim comes from a
   per-monitor edge vignette below, which leaves the centre fully transparent.
   The crosshair is a global system cursor (placement.rs), not a CSS cursor —
   a click-through window receives no WM_SETCURSOR. */
.overlay.active {
  cursor: crosshair;
}

/* The per-monitor "UP-TAKE has control" signal: a thin accent frame with a very
   subtle glow (§2.1 design language), plus a dark edge vignette that fades to a
   clear centre — the "framed and focused" feel of the old tint without covering
   the content being placed over. Drawn per monitor rather than around the whole
   desktop so it never lands in a dead zone between monitors (F-13). Purely an
   indicator — never intercepts input. */
.monitor-frame {
  position: absolute;
  box-sizing: border-box;
  border: 1.5px solid rgba(120, 180, 255, 0.55);
  border-radius: 6px;
  box-shadow:
    0 0 8px rgba(120, 180, 255, 0.35),
    inset 0 0 2px rgba(120, 180, 255, 0.35),
    inset 0 0 110px rgba(0, 0, 0, 0.32);
  pointer-events: none;
}

/* A persistent area (ADR-0009): a solid accent border over live content, with a
   faint fill so an empty region is still discernible against a busy desktop.
   Task 1.6 ships the Default type only (R-17); per-area chrome and the input
   routing that makes it interactive land in 1.6c. Never intercepts input — the
   overlay is click-through and stays that way. */
.area {
  left: 0;
  top: 0;
  position: absolute;
  box-sizing: border-box;
  border: 1.5px solid rgba(120, 180, 255, 0.9);
  border-radius: 4px;
  background: rgba(120, 180, 255, 0.06);
  box-shadow: 0 0 6px rgba(120, 180, 255, 0.3);
  pointer-events: none;
}

/* An area being moved or resized is not drawn at its old position at all — the
   dashed preview *is* the area for the duration of the gesture, so there is
   exactly one rectangle on screen per area at every moment. A leftover outline
   was tried first and still read as a second thing to look at. Nothing is
   stored to undo: the source is derived from the live gesture, so cancelling or
   interrupting a drag brings the area straight back where it was. */

/* The hovered area in Placement: brighter, so which area a drag will grab is
   visible before the button goes down. The cursor shape says *what* the drag
   will do (move, resize, dismiss) — that half is a system cursor set by
   placement.rs, because a click-through window receives no WM_SETCURSOR. */
.area.hovered {
  border-color: rgba(160, 210, 255, 1);
  background: rgba(120, 180, 255, 0.12);
  box-shadow: 0 0 10px rgba(120, 180, 255, 0.5);
}

/* A pinned tier (ADR-0013) is a state the user set and must be able to see; an
   `Auto` area shows nothing, since that is the default and marking it would be
   noise on every area. */
.layer-badge {
  position: absolute;
  left: 4px;
  top: 2px;
  font: 11px/1 system-ui, sans-serif;
  color: rgba(160, 210, 255, 0.95);
  text-shadow: 0 0 3px rgba(0, 0, 0, 0.8);
}

/* One-shot acknowledgement that a Copy or Save landed (F-35's success half).
   A single impulse that eases out rather than a pulse or a persistent badge:
   the point is "that worked", which is over the moment it is understood, and
   anything that lingers becomes chrome on a workspace meant to stay quiet.
   `forwards` leaves it at zero opacity; it costs one invisible span. */
.flash {
  position: absolute;
  inset: 0;
  border-radius: 3px;
  background: rgba(190, 225, 255, 0.85);
  pointer-events: none;
  animation: uptake-flash 420ms cubic-bezier(0.16, 1, 0.3, 1) forwards;
}

@keyframes uptake-flash {
  from {
    opacity: 0.9;
  }
  to {
    opacity: 0;
  }
}

/* Respect the OS setting: an unexpected flash is exactly the kind of motion
   `prefers-reduced-motion` exists for. Reduced to a brief static tint that
   still answers "did it work?" without the transition. */
@media (prefers-reduced-motion: reduce) {
  .flash {
    animation-duration: 900ms;
    animation-timing-function: steps(2, end);
  }
}

/* The armed-type cue (ADR-0018 §3), per monitor like the rest of the placement
   chrome (F-13). Sized and contrasted to be noticed rather than discovered: the
   ADR's own "makes hard" section says a weak indicator turns this design back
   into the "which mode am I in?" problem at one-drag scale, so understating it
   here would be undoing the decision. */
.armed-badge {
  position: absolute;
  left: 50%;
  top: 12px;
  transform: translateX(-50%);
  padding: 4px 12px;
  border-radius: 999px;
  background: rgba(120, 180, 255, 0.92);
  color: rgba(10, 20, 35, 0.95);
  font: 600 13px/1 system-ui, sans-serif;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  box-shadow: 0 2px 10px rgba(0, 0, 0, 0.45);
}

/* A Screenshot area's captured pixels, filling the area exactly (ADR-0014 §6).
   `object-fit: fill` rather than `contain`: the capture was taken *at* the
   area's rectangle, so the two always share an aspect ratio, and `contain`
   would letterbox on a rounding difference instead of showing it. */
.pin {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  object-fit: fill;
  border-radius: 3px;
  pointer-events: none;
  user-select: none;
}

/* The close control. Positioned from the rectangle Rust hit-tests — never from
   a layout computed here — so what is drawn and what is clickable are the same
   rectangle by construction. Revealed on hover only: a persistent ✕ on every
   area would be permanent clutter over the user's screen. */
.close {
  left: 0;
  top: 0;
  position: absolute;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  justify-content: center;
  font: 14px/1 system-ui, sans-serif;
  color: rgba(255, 255, 255, 0.95);
  background: rgba(190, 70, 80, 0.9);
  /* Uniformly rounded rather than tucked into a corner: on a small area this
     control sits *outside* the area, and at whichever of the four corners is
     actually on a monitor, so it has no fixed corner to be shaped for. */
  border-radius: 3px;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.5);
  pointer-events: none;
}

/* The live selection box while dragging out a new area: a dashed rubber-band so
   it reads as in-progress rather than committed. Fed from the mouse hook via the
   poll at ~60 Hz. */
.selection {
  left: 0;
  top: 0;
  /* The one element that moves every frame during a drag, so it is the one that
     gets promoted: will-change keeps it on its own compositor layer, and a move
     then costs a composite instead of a layout of the whole overlay. Applied
     here and nowhere else — promoting every area would trade that layout cost
     for GPU memory proportional to how many areas exist. */
  will-change: transform;
  position: absolute;
  box-sizing: border-box;
  border: 1.5px dashed rgba(150, 200, 255, 0.95);
  border-radius: 4px;
  background: rgba(120, 180, 255, 0.12);
  pointer-events: none;
}

/* The per-area menu (ADR-0013). Drawn here, hit-tested in placement.rs against
   the same rectangles — the overlay is click-through, so this is a picture of a
   menu that Rust makes behave like one. Rows are absolutely positioned from the
   rects Rust sent rather than flowed inside the panel, so a row can never be
   drawn anywhere other than where a click on it is detected. */
.menu {
  position: absolute;
  box-sizing: border-box;
  background: rgba(24, 28, 36, 0.96);
  border: 1px solid rgba(120, 180, 255, 0.45);
  border-radius: 6px;
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.5);
  pointer-events: none;
}

.menu-item {
  position: absolute;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 0 10px;
  font: 13px/1 system-ui, sans-serif;
  color: rgba(235, 240, 250, 0.95);
  pointer-events: none;
}

.menu-item.hovered {
  background: rgba(120, 180, 255, 0.22);
}

.menu-item .tick {
  width: 12px;
  color: rgba(160, 210, 255, 1);
}
</style>
