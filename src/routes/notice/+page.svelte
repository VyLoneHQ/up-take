<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { onMount } from 'svelte';
import { flip } from 'svelte/animate';
import { cubicInOut, cubicOut } from 'svelte/easing';
import { scale } from 'svelte/transition';
import {
  type Card,
  cardText,
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
import { TONES } from '$lib/tones';

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
  // Rust remembers who has the keyboard, so a click can give it back.
  void invoke('notice_hover').catch(() => {});
  held = true;
  stack = hold(stack, Date.now());
}

function leave(): void {
  held = false;
  stack = release(stack, Date.now());
}

/**
 * A card that goes. It fades a little ahead of the rest of the motion and
 * drifts aside, while its slot gives its height back, so the cards below
 * glide up into its place as it leaves rather than jumping once it is gone.
 * The slot clips only while it collapses, so a card's shadow is never cut
 * while it shows.
 */
function collapse(node: HTMLElement) {
  const height = node.offsetHeight;
  const gap = Number.parseFloat(getComputedStyle(node).marginBottom) || 0;
  return {
    duration: MOVE_MS + 40,
    easing: cubicInOut,
    css: (t: number) => `
      overflow: hidden;
      opacity: ${t * t};
      height: ${t * height}px;
      margin-bottom: ${t * gap}px;
      transform: translateX(${(1 - t) * 18}px);
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
// list's height otherwise. It GROWS at once, so a new card is never clipped,
// and SHRINKS only once the stack has stopped moving: resizing a window lands
// a frame or two late, so shrinking along with a leaving card made the edge
// chase the animation and cut into it (the founder's rig, 2026-11-03).
let sent = -1;
let frame = 0;
let shrink: ReturnType<typeof setTimeout> | undefined;
function measure(): number {
  return list && list.childElementCount > 0 ? Math.ceil(list.offsetHeight) : 0;
}
function send(height: number): void {
  if (height === sent) return;
  sent = height;
  void invoke('notice_resize', { height }).catch(() => {});
}
function report(): void {
  cancelAnimationFrame(frame);
  frame = requestAnimationFrame(() => {
    const height = measure();
    if (height > sent) {
      clearTimeout(shrink);
      send(height);
    } else if (height < sent) {
      clearTimeout(shrink);
      shrink = setTimeout(() => send(measure()), MOVE_MS + 80);
    }
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
    clearTimeout(shrink);
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
        style="--tone: {TONES[card.tone]}"
        title={cardText(card)}
        onclick={() => {
          stack = dismiss(stack, card.id);
        }}
      >
        <span class="dot" aria-hidden="true">●</span>
        <span class="line">
          {#if card.name}<span class="name">{card.name}</span><span
              class="sep"
              aria-hidden="true">//</span
            >{/if}<span class="what">{card.areaType}<span class="dot-sep"
              >·</span
            >{card.event}</span>
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
  /* Room for the cards' shadow, inside the window. */
  padding: 6px 10px 6px;
}

.slot {
  margin-bottom: 8px;
  border-radius: 10px;
}

.card {
  box-sizing: border-box;
  position: relative;
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  min-height: 46px;
  padding: 11px 12px 13px 14px;
  border: 1px solid rgba(var(--tone), 0.45);
  border-radius: 10px;
  background: rgba(22, 24, 28, 0.94);
  box-shadow:
    0 4px 14px rgba(0, 0, 0, 0.4),
    inset 3px 0 0 rgba(var(--tone), 0.85);
  color: rgba(235, 238, 240, 0.98);
  font: 13px/1.3 system-ui, sans-serif;
  text-align: left;
  cursor: pointer;
  overflow: hidden;
  transition:
    border-color 160ms ease-out,
    background-color 160ms ease-out;
}

.card:hover {
  border-color: rgba(var(--tone), 0.8);
  background: rgba(28, 31, 36, 0.96);
}

.dot {
  flex: none;
  color: rgba(var(--tone), 1);
  font-size: 12px;
}

/* "<name> // <area type> · <event>", on one line: the name is what the eye
   looks for, so it carries the colour and the weight. */
.line {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.name {
  font-weight: 600;
  color: rgba(var(--tone), 1);
}

.sep {
  margin: 0 7px;
  color: rgba(235, 238, 240, 0.35);
}

.what {
  color: rgba(235, 238, 240, 0.86);
}

.dot-sep {
  margin: 0 6px;
  color: rgba(235, 238, 240, 0.45);
}

/* How many times this kind fired while its card showed. */
.count {
  flex: none;
  min-width: 22px;
  padding: 2px 8px;
  border-radius: 999px;
  background: rgba(var(--tone), 0.16);
  border: 1px solid rgba(var(--tone), 0.5);
  color: rgba(var(--tone), 1);
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
  background: rgba(var(--tone), 0.6);
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
