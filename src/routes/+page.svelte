<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { onMount } from 'svelte';
import {
  type AreasPayload,
  escapeOverlay,
  monitorFramesCss,
  type Origin,
  type OverlayStateName,
  type PhysRect,
  physRectsToCss,
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
let areas: PhysRect[] = $state([]);
let selection: PhysRect | null = $state(null);
// The WebView owns its scale (ADR-0011); refreshed on every state event in case
// the overlay moved to a monitor at a different DPI.
let dpr = $state(1);

const frames: CssRect[] = $derived(monitorFramesCss(monitors, origin, dpr));
const areaFrames: CssRect[] = $derived(physRectsToCss(areas, origin, dpr));
// The selection box is only meaningful while placing; guarding on the state as
// well as the payload keeps a stale rectangle from lingering after a transition.
const selectionFrame: CssRect | null = $derived(
  overlayState === 'placement' ? physRectToCss(selection, origin, dpr) : null,
);

function onKeydown(event: KeyboardEvent) {
  if (!isDismissKey(event.key)) return;
  void escapeOverlay(invoke);
}

onMount(() => {
  const unlistenState = listen<StatePayload>('overlay://state', (event) => {
    overlayState = event.payload.state;
    monitors = event.payload.monitors;
    origin = event.payload.origin;
    dpr = window.devicePixelRatio;
    // A hidden overlay is drawing nothing; drop any half-finished selection so
    // it cannot reappear on the next show before the poll clears it.
    if (overlayState === 'hidden') selection = null;
  });
  const unlistenAreas = listen<AreasPayload>('overlay://areas', (event) => {
    areas = event.payload.areas;
  });
  const unlistenSelection = listen<SelectionPayload>(
    'placement://selection',
    (event) => {
      selection = event.payload.rect;
    },
  );

  // Request the current state only *after* the listeners are registered.
  // `listen` resolves once the backend has recorded the subscription; requesting
  // before that races the reply and drops it — which is exactly the startup
  // case, where the overlay is already in Placement (with areas possibly already
  // present) when the webview mounts. Chaining on the promise closes the gap.
  const ready = Promise.all([unlistenState, unlistenAreas, unlistenSelection]);
  void ready.then(() => invoke('overlay_request_state'));
  return () => {
    void ready.then(([un1, un2, un3]) => {
      un1();
      un2();
      un3();
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
    {#each areaFrames as area (`${area.x},${area.y},${area.width},${area.height}`)}
      <div
        class="area"
        style="left: {area.x}px; top: {area.y}px; width: {area.width}px; height: {area.height}px"
      ></div>
    {/each}
  {/if}

  {#if selectionFrame}
    <div
      class="selection"
      style="left: {selectionFrame.x}px; top: {selectionFrame.y}px; width: {selectionFrame.width}px; height: {selectionFrame.height}px"
    ></div>
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
</style>
