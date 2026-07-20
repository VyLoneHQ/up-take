<script lang="ts">
import { invoke } from '@tauri-apps/api/core';

// Presentation only (architecture §1): the frontend renders state and emits
// intents. Esc emits the hide intent; the decision happens in Rust.
async function onKeydown(event: KeyboardEvent) {
  if (event.key !== 'Escape') return;
  try {
    await invoke('overlay_hide');
  } catch (error) {
    // Esc is currently the only dismiss path, and the overlay covers every
    // monitor without click-through, so an unhandled rejection here strands
    // the user behind a window they cannot click past. Logging is the floor;
    // user-facing error reporting lands with task 1.15.
    console.error('Failed to hide the overlay:', error);
  }
}
</script>

<svelte:window onkeydown={onKeydown} />

<main class="overlay">
  <p class="hint">UP-TAKE — press Esc to dismiss</p>
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
  border-radius: 999px;
  background: rgba(0, 0, 0, 0.6);
  color: #fff;
  font-family: system-ui, sans-serif;
  font-size: 0.9rem;
}
</style>
