<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { onMount, tick } from 'svelte';
import {
  isPanelView,
  PANEL_EVENT,
  type PanelView,
  type PanelWatch,
} from '$lib/panel';
import { isLanguage, type Language, text } from '$lib/strings';
import { TONES, toneOf } from '$lib/tones';

// The area settings panel (roadmap 1.60). Rust shows this window beside its
// area; this page draws the area's settings and sets them. A click into it
// pins it open; Esc closes it.

const NAME_LIMIT = 40;

let language = $state<Language>('en');
let view = $state<PanelView | null>(null);
let name = $state('');
let editing = false;
let shell: HTMLDivElement | undefined = $state();

async function load(): Promise<void> {
  let next: unknown;
  try {
    next = await invoke('panel_view');
  } catch {
    return;
  }
  if (!isPanelView(next)) {
    view = null;
    return;
  }
  // Rust starts every panel it opens at a first guess, and this page outlives
  // the panel (its window is hidden, not closed), so an unchanged height still
  // has to be said once after every read: reporting only changes left the
  // second panel at the guess, cut off below its watch buttons (I-479).
  sent = 0;
  // A name being typed is not overwritten by a refresh of the same area.
  if (!editing || view?.id !== next.id) name = next.name ?? '';
  view = next;
  await tick();
  report();
}

function pin(): void {
  void invoke('panel_pin').catch(() => {});
}

function close(): void {
  void invoke('panel_close').catch(() => {});
}

function saveName(): void {
  editing = false;
  void invoke('panel_set_name', { name }).catch(() => {});
}

function setWatch(watch: PanelWatch): void {
  void invoke('panel_set_watch', { watch }).catch(() => {});
}

function setHeld(held: boolean): void {
  void invoke('panel_set_held', { held }).catch(() => {});
}

function onNameKey(event: KeyboardEvent): void {
  if (event.key === 'Enter') {
    event.preventDefault();
    saveName();
    (event.currentTarget as HTMLInputElement).blur();
  }
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') {
    event.preventDefault();
    if (editing) saveName();
    close();
  }
}

/** The margin around the panel inside its window, for its shadow (CSS). */
const MARGIN = 12;

let sent = 0;
function report(): void {
  const height = shell ? Math.ceil(shell.offsetHeight) + 2 * MARGIN : 0;
  if (height <= 2 * MARGIN || height === sent) return;
  sent = height;
  void invoke('panel_resize', { height }).catch(() => {});
}

// The panel exists only once its area has been read, so it is measured from
// then on: watching it from mount watched nothing, and the window kept its
// first guess and cut the panel off (the founder's rig, 2026-11-03).
$effect(() => {
  if (!shell) return;
  const observer = new ResizeObserver(report);
  observer.observe(shell);
  report();
  return () => observer.disconnect();
});

onMount(() => {
  void invoke('overlay_language')
    .then((chosen) => {
      if (isLanguage(chosen)) language = chosen;
    })
    .catch(() => {});
  const ready = listen(PANEL_EVENT, () => {
    void load();
  });
  void load();
  return () => {
    void ready.then((unlisten) => unlisten());
  };
});
</script>

<svelte:window onkeydown={onKeydown} />

{#if view}
  {@const tone = TONES[toneOf(view.kind)]}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="panel"
    style="--tone: {tone}"
    bind:this={shell}
    onpointerdown={pin}
  >
    <header>
      <span class="swatch" aria-hidden="true"></span>
      <span class="kind">{text(language, `kind.${view.kind}` as never)}</span>
      <button
        type="button"
        class="close"
        title={text(language, 'panel.close')}
        aria-label={text(language, 'panel.close')}
        onclick={close}>×</button
      >
    </header>

    <label class="field">
      <span class="label">{text(language, 'panel.name')}</span>
      <input
        type="text"
        maxlength={NAME_LIMIT}
        spellcheck="false"
        placeholder={text(language, 'rename.placeholder')}
        bind:value={name}
        onfocus={() => {
          editing = true;
          pin();
        }}
        onblur={saveName}
        onkeydown={onNameKey}
      />
    </label>

    {#if view.watch}
      <div class="field">
        <span class="label">{text(language, 'menu.watch')}</span>
        <div class="choice" role="radiogroup">
          <button
            type="button"
            role="radio"
            aria-checked={view.watch === 'changes'}
            class:on={view.watch === 'changes'}
            onclick={() => setWatch('changes')}
            >{text(language, 'menu.watch.changes')}</button
          >
          <button
            type="button"
            role="radio"
            aria-checked={view.watch === 'settles'}
            class:on={view.watch === 'settles'}
            onclick={() => setWatch('settles')}
            >{text(language, 'menu.watch.settles')}</button
          >
        </div>
      </div>
    {/if}

    {#if view.held !== null}
      <button
        type="button"
        class="hold"
        class:held={view.held}
        onclick={() => setHeld(!view?.held)}
      >
        <span aria-hidden="true">{view.held ? '▶' : '❚❚'}</span>
        {text(language, view.held ? 'menu.resume' : 'menu.pause')}
      </button>
    {/if}
  </div>
{/if}

<style>
:global(html),
:global(body) {
  margin: 0;
  background: transparent;
  overflow: hidden;
  font: 12px/1.3 system-ui, sans-serif;
  color: rgba(235, 238, 240, 0.96);
}

.panel {
  box-sizing: border-box;
  /* MARGIN in the script: room for the shadow inside the window. */
  margin: 12px;
  padding: 10px 12px 12px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  border: 1px solid rgba(var(--tone), 0.4);
  border-radius: 10px;
  background: rgba(22, 24, 28, 0.97);
  /* Fits inside the 12px margin: blur 10 at offset 3 reaches 13 below. */
  box-shadow:
    0 3px 9px rgba(0, 0, 0, 0.5),
    inset 3px 0 0 rgba(var(--tone), 0.85);
  animation: appear 160ms ease-out both;
}

@keyframes appear {
  from {
    opacity: 0;
    transform: translateY(-4px);
  }
  to {
    opacity: 1;
    transform: none;
  }
}

header {
  display: flex;
  align-items: center;
  gap: 8px;
}

.swatch {
  width: 10px;
  height: 10px;
  border-radius: 2px;
  border: 1.5px solid rgba(var(--tone), 0.9);
  background: rgba(var(--tone), 0.15);
}

.kind {
  flex: 1;
  font-weight: 600;
  color: rgba(var(--tone), 1);
}

.close {
  border: none;
  background: none;
  color: rgba(235, 238, 240, 0.6);
  font-size: 16px;
  line-height: 1;
  padding: 0 2px;
  cursor: pointer;
}

.close:hover {
  color: rgba(235, 238, 240, 1);
}

/* No focus ring on a click; a thin one in the type's colour from the
   keyboard. */
button:focus,
input:focus {
  outline: none;
}

button:focus-visible {
  outline: 1px solid rgba(var(--tone), 0.8);
  outline-offset: 1px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.label {
  font-size: 11px;
  color: rgba(235, 238, 240, 0.6);
}

input {
  box-sizing: border-box;
  width: 100%;
  padding: 5px 8px;
  border: 1px solid rgba(235, 238, 240, 0.18);
  border-radius: 6px;
  background: rgba(10, 11, 13, 0.6);
  color: inherit;
  font: inherit;
  outline: none;
}

input:focus {
  border-color: rgba(var(--tone), 0.8);
}

.choice {
  display: flex;
  gap: 4px;
}

.choice button,
.hold {
  flex: 1;
  padding: 5px 8px;
  border: 1px solid rgba(235, 238, 240, 0.16);
  border-radius: 6px;
  background: rgba(255, 255, 255, 0.04);
  color: rgba(235, 238, 240, 0.85);
  font: inherit;
  cursor: pointer;
  transition:
    background-color 120ms ease-out,
    border-color 120ms ease-out;
}

.choice button:hover,
.hold:hover {
  background: rgba(255, 255, 255, 0.08);
}

.choice button.on {
  border-color: rgba(var(--tone), 0.8);
  background: rgba(var(--tone), 0.16);
  color: rgba(var(--tone), 1);
}

.hold {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
}

.hold.held {
  border-color: rgba(var(--tone), 0.6);
  color: rgba(var(--tone), 1);
}
</style>
