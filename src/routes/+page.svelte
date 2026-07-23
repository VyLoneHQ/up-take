<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { onMount } from 'svelte';
import {
  type AreaFrame,
  type AreasPayload,
  type AreaView,
  areaFramesCss,
  dismissFocusedArea,
  escapeOverlay,
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
  physRectToCss,
  type SelectionPayload,
  type StatePayload,
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
// The WebView owns its scale (ADR-0011); refreshed on every state event in case
// the overlay moved to a monitor at a different DPI.
let dpr = $state(1);

const frames: CssRect[] = $derived(monitorFramesCss(monitors, origin, dpr));
// Hover chrome — the close control, the brighter border — belongs to Placement
// only: in Living the overlay does not own the pointer, so a control that
// appeared to follow the cursor would be one no click could reach.
const areaFrames: AreaFrame[] = $derived(
  areaFramesCss(
    areas,
    origin,
    dpr,
    overlayState === 'placement' ? hoveredArea : null,
    overlayState === 'placement' ? draggedArea : null,
  ),
);
// The selection box is only meaningful while placing; guarding on the state as
// well as the payload keeps a stale rectangle from lingering after a transition.
const selectionFrame: CssRect | null = $derived(
  overlayState === 'placement' ? physRectToCss(selection, origin, dpr) : null,
);
const menuFrame: MenuFrame | null = $derived(
  overlayState === 'placement' ? menuFrameCss(menu, origin, dpr) : null,
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
  }
}

onMount(() => {
  const unlistenState = listen<StatePayload>('overlay://state', (event) => {
    overlayState = event.payload.state;
    monitors = event.payload.monitors;
    origin = event.payload.origin;
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
  });
  const unlistenSelection = listen<SelectionPayload>(
    'placement://selection',
    (event) => {
      selection = event.payload.rect;
      draggedArea = event.payload.source;
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

<main class="overlay" class:active={showsTint(overlayState)}>
  {#if showsTint(overlayState)}
    {#each frames as frame (`${frame.x},${frame.y},${frame.width},${frame.height}`)}
      <div
        class="monitor-frame"
        style="left: {frame.x}px; top: {frame.y}px; width: {frame.width}px; height: {frame.height}px"
      ></div>
    {/each}
  {/if}

  {#if overlayState !== 'hidden'}
    {#each areaFrames as area (area.id)}
      <div
        class="area"
        class:hovered={area.hovered}
        class:source={area.source}
        class:pinned={area.layer !== 'auto'}
        style="left: {area.rect.x}px; top: {area.rect.y}px; width: {area.rect
          .width}px; height: {area.rect.height}px"
      >
        {#if area.layer !== 'auto'}
          <span class="layer-badge">{area.layer === 'front' ? '▲' : '▼'}</span>
        {/if}
      </div>
      {#if area.hovered}
        <div
          class="close"
          style="left: {area.close.x}px; top: {area.close.y}px; width: {area
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
      style="left: {selectionFrame.x}px; top: {selectionFrame.y}px; width: {selectionFrame.width}px; height: {selectionFrame.height}px"
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
  position: absolute;
  box-sizing: border-box;
  border: 1.5px solid rgba(120, 180, 255, 0.9);
  border-radius: 4px;
  background: rgba(120, 180, 255, 0.06);
  box-shadow: 0 0 6px rgba(120, 180, 255, 0.3);
  pointer-events: none;
}

/* The area a move or resize started from, while the drag is live. Drawn as a
   faint grey outline with no fill so it reads as "this is where it is coming
   from" rather than as a second area — a solid copy left behind is the thing
   that made a move look like a duplication. Purely derived from the live
   gesture, so a cancelled or interrupted drag restores the normal appearance
   with no undo path of its own. */
.area.source {
  border: 1.5px dashed rgba(190, 195, 205, 0.5);
  background: transparent;
  box-shadow: none;
}

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

/* The close control. Positioned from the rectangle Rust hit-tests — never from
   a layout computed here — so what is drawn and what is clickable are the same
   rectangle by construction. Revealed on hover only: a persistent ✕ on every
   area would be permanent clutter over the user's screen. */
.close {
  position: absolute;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  justify-content: center;
  font: 14px/1 system-ui, sans-serif;
  color: rgba(255, 255, 255, 0.95);
  background: rgba(190, 70, 80, 0.85);
  border-radius: 0 3px 0 4px;
  pointer-events: none;
}

/* The live selection box while dragging out a new area: a dashed rubber-band so
   it reads as in-progress rather than committed. Fed from the mouse hook via the
   poll at ~60 Hz. */
.selection {
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
