<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { onMount } from 'svelte';
import {
  escapeOverlay,
  monitorFramesCss,
  type OverlayStateName,
  type StatePayload,
  showsTint,
} from '$lib/overlay-state';
import { type CssRect, isDismissKey } from '$lib/regions';

// Presentation only (architecture §1): the Rust side owns the state machine
// (ADR-0012) and emits the current state; this component renders the focus
// indicator for it and emits the Esc intent. No decision is made here.
let overlayState: OverlayStateName = $state('hidden');
let frames: CssRect[] = $state([]);

function onKeydown(event: KeyboardEvent) {
  if (!isDismissKey(event.key)) return;
  void escapeOverlay(invoke);
}

onMount(() => {
  // Attach the listener first, then ask Rust for the current state — a webview
  // that mounted *after* the startup summon (or a dev reload) would otherwise
  // show no indicator until the next transition.
  const unlisten = listen<StatePayload>('overlay://state', (event) => {
    overlayState = event.payload.state;
    // `devicePixelRatio` is read here, not sent by Rust: the WebView owns its
    // scale (ADR-0011).
    frames = monitorFramesCss(
      event.payload.monitors,
      event.payload.origin,
      window.devicePixelRatio,
    );
  });
  void invoke('overlay_request_state');
  return () => {
    void unlisten.then((un) => un());
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

/* PLACEMENT: UP-TAKE has input focus (ADR-0012). A *light* tint — lighter than
   a modal capture veil — so the user can still see the screen content they are
   placing areas over. The crosshair marks the surface as draggable (slice 2). */
.overlay.active {
  background: rgba(0, 0, 0, 0.12);
  cursor: crosshair;
}

/* The per-monitor "UP-TAKE has control" signal: a thin accent frame with a very
   subtle glow (§2.1 design language). Drawn per monitor rather than around the
   whole desktop so it never lands in a dead zone between monitors (F-13).
   Purely an indicator — never intercepts input. */
.monitor-frame {
  position: absolute;
  box-sizing: border-box;
  border: 1.5px solid rgba(120, 180, 255, 0.55);
  border-radius: 6px;
  box-shadow:
    0 0 8px rgba(120, 180, 255, 0.35),
    inset 0 0 8px rgba(120, 180, 255, 0.15);
  pointer-events: none;
}
</style>
