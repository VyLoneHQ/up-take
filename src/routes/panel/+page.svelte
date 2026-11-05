<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { onMount, tick } from 'svelte';
import {
  DEPTHS,
  isPanelView,
  ON_RESIZE,
  PANEL_EVENT,
  type PanelDepth,
  type PanelOnResize,
  type PanelView,
  type PanelWatch,
} from '$lib/panel';
import { isLanguage, type Language, text } from '$lib/strings';
import { TONES, toneOf } from '$lib/tones';

// The area settings panel (roadmap 1.60). Rust shows this window beside its
// area; this page draws the area's settings and sets them. Esc closes it.

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

// The menu's other rows (roadmap 1.60, second slice): each calls the command
// that runs what the matching menu row runs.
function set(command: string, args: Record<string, unknown>): void {
  void invoke(command, args).catch(() => {});
}

const depthLabel: Record<PanelDepth, string> = {
  front: 'menu.depth.front',
  auto: 'menu.depth.auto',
  back: 'menu.depth.back',
};

const onResizeLabel: Record<PanelOnResize, string> = {
  scale: 'menu.on_resize.scale',
  scale_position: 'menu.on_resize.scale_position',
  corner: 'menu.on_resize.corner',
  top_left: 'menu.on_resize.top_left',
};

/** A switch toggled from the keyboard as well as by a click. */
function onSwitchKey(event: KeyboardEvent, flip: () => void): void {
  if (event.key === ' ' || event.key === 'Enter') {
    event.preventDefault();
    flip();
  }
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
  <div
    class="panel"
    style="--tone: {tone}"
    bind:this={shell}
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
        }}
        onblur={saveName}
        onkeydown={onNameKey}
      />
    </label>

    <div class="field">
      <span class="label">{text(language, 'menu.area_type')}</span>
      <div class="choice wrap" role="radiogroup">
        {#each view.kinds as kind (kind)}
          <button
            type="button"
            role="radio"
            aria-checked={view.kind === kind}
            class:on={view.kind === kind}
            onclick={() => set('panel_set_type', { kind })}
            >{text(language, `kind.${kind}` as never)}</button
          >
        {/each}
      </div>
    </div>

    {#if view.ocr}
      <div class="field">
        <span class="label"
          >{text(language, 'settings.ocr_behaviour.name')}</span
        >
        <div class="choice" role="radiogroup">
          {#each ['in_place', 'rendered'] as const as behaviour (behaviour)}
            <button
              type="button"
              role="radio"
              aria-checked={view.ocr === behaviour}
              class:on={view.ocr === behaviour}
              onclick={() => set('panel_set_ocr', { behaviour })}
              >{text(language, `settings.ocr_behaviour.${behaviour}`)}</button
            >
          {/each}
        </div>
      </div>
    {/if}

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

    <div class="rule" aria-hidden="true"></div>

    <div class="field">
      <span class="label">{text(language, 'menu.depth')}</span>
      <div class="choice" role="radiogroup">
        {#each DEPTHS as depth (depth)}
          <button
            type="button"
            role="radio"
            aria-checked={view.depth === depth}
            class:on={view.depth === depth}
            onclick={() => set('panel_set_depth', { depth })}
            >{text(language, depthLabel[depth] as never)}</button
          >
        {/each}
      </div>
    </div>

    <div
      class="toggle"
      class:on={view.sticky}
      class:disabled={!view.can_stick}
      role="switch"
      tabindex={view.can_stick ? 0 : -1}
      aria-checked={view.sticky}
      aria-disabled={!view.can_stick}
      onclick={() => {
        if (view?.can_stick) set('panel_set_sticky', { sticky: !view.sticky });
      }}
      onkeydown={(event) =>
        onSwitchKey(event, () => {
          if (view?.can_stick)
            set('panel_set_sticky', { sticky: !view.sticky });
        })}
    >
      <span>{text(language, 'menu.sticky')}</span>
      <span class="switch" aria-hidden="true"></span>
    </div>

    {#if view.on_resize}
      <div class="field sub">
        <span class="label">{text(language, 'menu.on_resize')}</span>
        <div class="choice wrap" role="radiogroup">
          {#each ON_RESIZE as onResize (onResize)}
            <button
              type="button"
              role="radio"
              aria-checked={view.on_resize === onResize}
              class:on={view.on_resize === onResize}
              onclick={() =>
                set('panel_set_on_resize', { onResize })}
              >{text(language, onResizeLabel[onResize] as never)}</button
            >
          {/each}
        </div>
      </div>
    {/if}

    <div
      class="toggle"
      class:on={view.click_through}
      role="switch"
      tabindex="0"
      aria-checked={view.click_through}
      onclick={() =>
        set('panel_set_click_through', { on: !view?.click_through })}
      onkeydown={(event) =>
        onSwitchKey(event, () =>
          set('panel_set_click_through', { on: !view?.click_through }),
        )}
    >
      <span>{text(language, 'menu.click_through')}</span>
      <span class="switch" aria-hidden="true"></span>
    </div>

    {#if view.capture}
      <div class="rule" aria-hidden="true"></div>
      <div class="actions">
        <button
          type="button"
          class="act"
          onclick={() => set('panel_export', { save: false })}
          >{text(language, 'menu.copy')}</button
        >
        <button
          type="button"
          class="act"
          onclick={() => set('panel_export', { save: true })}
          >{text(language, 'menu.save_image')}</button
        >
      </div>
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

/* The menu's other rows (second slice), as in the mock he approved on
   2026-11-05. */
.choice.wrap {
  flex-wrap: wrap;
}

.choice.wrap button {
  flex: 0 1 auto;
}

.rule {
  height: 1px;
  margin: 0 -12px;
  background: rgba(235, 238, 240, 0.08);
}

.toggle {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  cursor: pointer;
}

.toggle.disabled {
  cursor: default;
  opacity: 0.45;
}

.toggle:focus {
  outline: none;
}

.toggle:focus-visible {
  outline: 1px solid rgba(var(--tone), 0.8);
  outline-offset: 2px;
}

.switch {
  position: relative;
  width: 26px;
  height: 14px;
  border-radius: 7px;
  background: rgba(235, 238, 240, 0.16);
  transition: background-color 120ms ease-out;
}

.switch::after {
  content: '';
  position: absolute;
  top: 2px;
  left: 2px;
  width: 10px;
  height: 10px;
  border-radius: 50%;
  background: rgba(235, 238, 240, 0.85);
  transition: transform 120ms ease-out;
}

.toggle.on .switch {
  background: rgba(var(--tone), 0.6);
}

.toggle.on .switch::after {
  transform: translateX(12px);
  background: rgba(235, 238, 240, 0.96);
}

.sub {
  padding-left: 10px;
  border-left: 1px solid rgba(235, 238, 240, 0.08);
}

.actions {
  display: flex;
  gap: 4px;
}

.act {
  flex: 1;
  padding: 5px 8px;
  border: 1px solid rgba(235, 238, 240, 0.16);
  border-radius: 6px;
  background: rgba(255, 255, 255, 0.04);
  color: rgba(235, 238, 240, 0.85);
  font: inherit;
  cursor: pointer;
}

.act:hover {
  background: rgba(255, 255, 255, 0.08);
}

@media (prefers-reduced-motion: reduce) {
  .switch,
  .switch::after {
    transition: none;
  }
}
</style>
