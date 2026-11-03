<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { onMount } from 'svelte';
import {
  FADE_MS,
  isNotice,
  LINGER_MS,
  NOTICE_EVENT,
  type NoticePayload,
} from '$lib/notice';

// UP-TAKE's own notification card (ADR-0054). Rust shows this window without
// focus and hands it the card; this page draws it, fades it after a while
// unless the pointer rests on it, and closes it on a click.

let card = $state<NoticePayload | null>(null);
let fading = $state(false);
let hovered = false;
let timer: ReturnType<typeof setTimeout> | undefined;

function arm(): void {
  clearTimeout(timer);
  timer = setTimeout(() => {
    if (!hovered) close();
  }, LINGER_MS);
}

function show(next: NoticePayload): void {
  card = next;
  fading = false;
  arm();
}

function close(): void {
  const closing = card;
  if (!closing) return;
  clearTimeout(timer);
  fading = true;
  setTimeout(() => {
    // A newer card may have arrived during the fade: leave it be.
    if (card?.serial !== closing.serial) return;
    card = null;
    fading = false;
    void invoke('notice_dismiss', { serial: closing.serial }).catch(() => {});
  }, FADE_MS);
}

onMount(() => {
  const ready = listen(NOTICE_EVENT, (event) => {
    if (isNotice(event.payload)) show(event.payload);
  });
  // The first card can be sent before this page has loaded.
  void invoke('notice_current')
    .then((current) => {
      if (isNotice(current) && card === null) show(current);
    })
    .catch(() => {});
  return () => {
    clearTimeout(timer);
    void ready.then((unlisten) => unlisten());
  };
});
</script>

{#if card}
  <button
    type="button"
    class="card"
    class:fading
    style="--fade: {FADE_MS}ms"
    onclick={close}
    onpointerenter={() => {
      hovered = true;
    }}
    onpointerleave={() => {
      hovered = false;
      arm();
    }}
  >
    <span class="dot" aria-hidden="true">●</span>
    <span class="text">
      <span class="title">{card.title}</span>
      <span class="body">{card.body}</span>
    </span>
  </button>
{/if}

<style>
:global(html),
:global(body) {
  margin: 0;
  background: transparent;
  overflow: hidden;
}

.card {
  box-sizing: border-box;
  position: absolute;
  inset: 6px;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 0 16px;
  border: 1px solid rgba(100, 210, 150, 0.55);
  border-radius: 10px;
  background: rgba(22, 24, 28, 0.94);
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.45);
  color: rgba(235, 238, 240, 0.98);
  font: 13px/1.3 system-ui, sans-serif;
  text-align: left;
  cursor: pointer;
  opacity: 1;
  transition: opacity var(--fade) ease-out;
}

.card.fading {
  opacity: 0;
}

.dot {
  color: rgba(140, 235, 180, 0.98);
  font-size: 14px;
}

.text {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.title {
  font-weight: 600;
  font-size: 12px;
  color: rgba(140, 235, 180, 0.98);
}

.body {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
