<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { onMount } from 'svelte';
import { flip } from 'svelte/animate';
import { cubicOut } from 'svelte/easing';
import { scale } from 'svelte/transition';
import {
  type Card,
  dismiss,
  expire,
  hold,
  isNotice,
  LINGER_MS,
  NOTICE_EVENT,
  nextExpiry,
  receive,
  release,
} from '$lib/notice';

// UP-TAKE's notification stack (ADR-0054; the rules and their tests are in
// $lib/notice). Rust shows this window without focus at the top centre of
// its monitor; this page draws the cards and sizes the window to them.

/** How long cards take to move, enter and leave. */
const MOVE_MS = 260;

let stack = $state<Card[]>([]);
let held = $state(false);
let nextId = 1;
let timer: ReturnType<typeof setTimeout> | undefined;
let list: HTMLDivElement | undefined = $state();

async function take(): Promise<void> {
  let cards: unknown;
  try {
    cards = await invoke('notice_take');
  } catch {
    return;
  }
  if (!Array.isArray(cards)) return;
  let next = stack;
  for (const card of cards) {
    if (!isNotice(card)) continue;
    next = receive(next, card, Date.now(), nextId, held);
    nextId += 1;
  }
  stack = next;
}

// Lets each card go at its time: one timer, for the next card due.
$effect(() => {
  clearTimeout(timer);
  const due = nextExpiry(stack);
  if (due === null) return;
  timer = setTimeout(
    () => {
      stack = expire(stack, Date.now());
    },
    Math.max(0, due - Date.now()),
  );
  return () => clearTimeout(timer);
});

function enter(): void {
  held = true;
  stack = hold(stack, Date.now());
}

function leave(): void {
  held = false;
  stack = release(stack, Date.now());
}

/**
 * A card that goes: it fades, slides a little up and gives its height back,
 * so the cards below glide into its place as it leaves rather than jumping
 * once it is gone.
 */
function collapse(node: HTMLElement) {
  const height = node.offsetHeight;
  const gap = Number.parseFloat(getComputedStyle(node).marginBottom) || 0;
  return {
    duration: MOVE_MS,
    easing: cubicOut,
    css: (t: number) => `
      opacity: ${t};
      height: ${t * height}px;
      margin-bottom: ${t * gap}px;
      transform: translateY(${(1 - t) * -6}px) scale(${0.97 + 0.03 * t});
    `,
  };
}

/** A new card: drops into the top place and fades in. */
function arrive(_node: HTMLElement) {
  return {
    duration: MOVE_MS,
    easing: cubicOut,
    css: (t: number) => `
      opacity: ${t};
      transform: translateY(${(1 - t) * -14}px) scale(${0.97 + 0.03 * t});
    `,
  };
}

// Size the window to the stack: zero (hidden) when nothing is drawn, the
// list's height otherwise. A leaving card gives its height back over its
// animation, so the window shrinks with it and nothing is clipped.
let sent = -1;
let frame = 0;
function report(): void {
  cancelAnimationFrame(frame);
  frame = requestAnimationFrame(() => {
    const height =
      list && list.childElementCount > 0 ? Math.ceil(list.offsetHeight) : 0;
    if (height === sent) return;
    sent = height;
    void invoke('notice_resize', { height }).catch(() => {});
  });
}

onMount(() => {
  const ready = listen(NOTICE_EVENT, () => {
    void take();
  });
  // Cards queued before this page had loaded.
  void take();
  const observer = new ResizeObserver(report);
  if (list) observer.observe(list);
  return () => {
    observer.disconnect();
    cancelAnimationFrame(frame);
    clearTimeout(timer);
    void ready.then((unlisten) => unlisten());
  };
});

// The list's own size does not change when its last card is removed with no
// animation left (its padding stays), so report after every change as well.
$effect(() => {
  void stack.length;
  report();
});
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="stack"
  class:held
  bind:this={list}
  onpointerenter={enter}
  onpointerleave={leave}
>
  {#each stack as card (card.id)}
    <div
      class="slot"
      animate:flip={{ duration: MOVE_MS, easing: cubicOut }}
      in:arrive
      out:collapse
    >
      <button
        type="button"
        class="card"
        onclick={() => {
          stack = dismiss(stack, card.id);
        }}
      >
        <span class="dot" aria-hidden="true">●</span>
        <span class="text">
          <span class="title">{card.title}</span>
          <span class="body">{card.body}</span>
        </span>
        {#if card.count > 1}
          {#key card.count}
            <span
              class="count"
              in:scale={{ start: 1.35, duration: 220, easing: cubicOut, opacity: 1 }}
              >×{card.count}</span
            >
          {/key}
        {/if}
        {#key card.expiresAt}
          <span
            class="life"
            style="--from: {Math.min(1, card.lifetime / LINGER_MS)}; animation-duration: {card.lifetime}ms"
          ></span>
        {/key}
      </button>
    </div>
  {/each}
</div>

<style>
:global(html),
:global(body) {
  margin: 0;
  background: transparent;
  overflow: hidden;
}

.stack {
  box-sizing: border-box;
  padding: 6px 6px 0;
}

.slot {
  margin-bottom: 8px;
  overflow: hidden;
  border-radius: 10px;
}

.card {
  box-sizing: border-box;
  position: relative;
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  min-height: 60px;
  padding: 10px 14px 12px 16px;
  border: 1px solid rgba(100, 210, 150, 0.45);
  border-radius: 10px;
  background: rgba(22, 24, 28, 0.94);
  box-shadow: 0 4px 14px rgba(0, 0, 0, 0.4);
  color: rgba(235, 238, 240, 0.98);
  font: 13px/1.3 system-ui, sans-serif;
  text-align: left;
  cursor: pointer;
  overflow: hidden;
  transition: border-color 160ms ease-out, background-color 160ms ease-out;
}

.card:hover {
  border-color: rgba(140, 235, 180, 0.8);
  background: rgba(28, 31, 36, 0.96);
}

.dot {
  flex: none;
  color: rgba(140, 235, 180, 0.98);
  font-size: 14px;
}

.text {
  display: flex;
  flex: 1;
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

/* How many times this kind fired while its card showed. */
.count {
  flex: none;
  min-width: 22px;
  padding: 2px 8px;
  border-radius: 999px;
  background: rgba(100, 210, 150, 0.18);
  border: 1px solid rgba(100, 210, 150, 0.5);
  color: rgba(160, 240, 195, 1);
  font: 600 12px/1.2 system-ui, sans-serif;
  font-variant-numeric: tabular-nums;
  text-align: center;
}

/* The time a card has left: a thin line along its bottom that runs out, and
   fills again when the card is triggered again. Paused while the pointer
   holds the stack. */
.life {
  position: absolute;
  left: 0;
  right: 0;
  bottom: 0;
  height: 2px;
  background: rgba(100, 210, 150, 0.55);
  transform-origin: left center;
  animation-name: run-out;
  animation-timing-function: linear;
  animation-fill-mode: both;
}

.held .life {
  animation-play-state: paused;
}

@keyframes run-out {
  from {
    transform: scaleX(var(--from));
  }
  to {
    transform: scaleX(0);
  }
}
</style>
