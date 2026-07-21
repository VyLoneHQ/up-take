<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { onMount } from 'svelte';

// Presentation only (architecture §1): the frontend renders state and emits
// intents. Esc and the pill's click emit the hide intent; measurement of the
// interactive regions is reported in CSS pixels and converted (and hit-tested)
// on the Rust side.
let pill: HTMLButtonElement;

async function hideOverlay() {
  try {
    await invoke('overlay_hide');
  } catch (error) {
    // Esc and the pill are the only dismiss paths; logging is the floor,
    // user-facing error reporting lands with task 1.15.
    console.error('Failed to hide the overlay:', error);
  }
}

function onKeydown(event: KeyboardEvent) {
  if (event.key !== 'Escape') return;
  void hideOverlay();
}

// The pill is the only interactive region for now (task 1.2): while the
// overlay is visible, the window takes input inside reported regions and lets
// clicks fall through everywhere else. Re-measured on every window resize —
// the overlay is resized to the virtual desktop on every show, which recenters
// the pill.
async function reportInteractiveRegions() {
  const rect = pill.getBoundingClientRect();
  try {
    await invoke('overlay_set_interactive_regions', {
      regions: [
        { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
      ],
    });
  } catch (error) {
    // Fail-safe by design: until a report succeeds the Rust side keeps the
    // whole window interactive, so a lost report costs click-through, never
    // the dismiss path.
    console.error('Failed to report interactive regions:', error);
  }
}

onMount(() => {
  void reportInteractiveRegions();
});
</script>

<svelte:window onkeydown={onKeydown} onresize={reportInteractiveRegions} />

<main class="overlay">
  <button type="button" class="hint" bind:this={pill} onclick={hideOverlay}>
    UP-TAKE — Esc or click here to dismiss
  </button>
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
  /* Faint tint so a shown overlay is visibly present; the drag-to-select UI
     (task 1.6) replaces this surface. */
  background: rgba(0, 0, 0, 0.25);
  cursor: crosshair;
  display: flex;
  align-items: flex-start;
  justify-content: center;
  user-select: none;
}

.hint {
  margin-top: 2rem;
  padding: 0.4rem 1rem;
  border: none;
  border-radius: 999px;
  background: rgba(0, 0, 0, 0.6);
  color: #fff;
  font-family: system-ui, sans-serif;
  font-size: 0.9rem;
  cursor: pointer;
}
</style>
