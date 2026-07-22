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
  // Request the current state only *after* the listener is registered.
  // `listen` resolves once the backend has recorded it; requesting before that
  // races the reply and drops it — which is exactly the startup case, where the
  // overlay is already in Placement when the webview mounts but its first emit
  // arrives before the listener exists. Chaining on the promise closes the gap.
  void unlisten.then(() => invoke('overlay_request_state'));
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

/* PLACEMENT: UP-TAKE has input focus (ADR-0012). No full-surface fill — a flat
   tint over a hardware video plane (YouTube etc.) punches it to solid grey, and
   placing an area over live content is a core use case. The dim comes from a
   per-monitor edge vignette below, which leaves the centre — where content and
   video are — fully transparent. The crosshair marks the surface as draggable
   (slice 2). */
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
</style>
