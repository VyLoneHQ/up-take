<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { onMount } from 'svelte';
import * as regions from '$lib/regions';

// Presentation only (architecture §1): the frontend renders state and emits
// intents. The logic lives in `$lib/regions` so it can be tested without a DOM
// harness — see `regions.test.ts`.
let pill: HTMLButtonElement;

function dismiss() {
  void regions.hideOverlay(invoke);
}

function onKeydown(event: KeyboardEvent) {
  if (!regions.isDismissKey(event.key)) return;
  dismiss();
}

// The pill is the only interactive region for now (task 1.2): while the overlay
// is visible, the window takes input inside reported regions and lets clicks
// fall through everywhere else.
//
// `devicePixelRatio` travels with the measurement because Rust cannot derive
// it reliably — tao's per-window scale factor can disagree with the one the
// WebView laid out in, which silently offsets every region. See the
// `reportInteractiveRegions` docs.
//
// `resize` is the only re-measurement trigger, and it covers a DPI change only
// indirectly — which is worth knowing before trusting it. Windows sends the
// window `WM_DPICHANGED`; tao responds by rescaling its physical size to
// preserve its *logical* size, so `devicePixelRatio` and the physical size move
// together, the CSS viewport is unchanged, and no `resize` fires here. The
// event that does fire is the one raised by the Rust side re-fitting the
// overlay to the full virtual desktop afterwards. See `reconvert_regions` in
// `click_through.rs` for the chain and for what breaks it.
function report() {
  void regions.reportInteractiveRegions(
    invoke,
    [pill],
    window.devicePixelRatio,
  );
}

onMount(report);
</script>

<svelte:window onkeydown={onKeydown} onresize={report} />

<main class="overlay">
  <button type="button" class="hint" bind:this={pill} onclick={dismiss}>
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
