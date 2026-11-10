<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { onMount, tick } from 'svelte';
import {
  CALM_STEPS_MS,
  DEPTHS,
  GAPS_MS,
  isPanelView,
  NOTIFY,
  nearestStep,
  ON_RESIZE,
  PANEL_EVENT,
  type PanelDepth,
  type PanelNotify,
  type PanelOnResize,
  type PanelTuning,
  type PanelView,
  type PanelWatch,
  SAMPLE_STEPS_MS,
  SHARE_STEPS_PERMILLE,
} from '$lib/panel';
import {
  fill,
  isLanguage,
  type Language,
  type TextKey,
  text,
} from '$lib/strings';
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

// Saved as it is typed, with the area's own id: a click outside closes the
// panel before the field loses focus, and a save sent only then was dropped,
// or could name another area whose panel opened meanwhile (review of #161,
// round 1).
function saveName(): void {
  if (view)
    void invoke('panel_set_name', { id: view.id, name }).catch(() => {});
}

function endEditing(): void {
  editing = false;
  saveName();
}

/** After a control was used: placing gets the keyboard back (review of
 * #161, round 1). The name field alone keeps it. */
function done(): void {
  void invoke('panel_return_keyboard').catch(() => {});
}

function setWatch(watch: PanelWatch): void {
  void invoke('panel_set_watch', { watch }).catch(() => {});
  done();
}

// A watch area's tuning (roadmap 1.65): sent whole, with one value changed;
// Rust keeps each in its range.
function setTuning(change: Partial<PanelTuning>): void {
  if (!view?.tuning) return;
  const tuning = { ...view.tuning, ...change };
  view.tuning = tuning;
  void invoke('panel_set_tuning', { tuning }).catch(() => {});
}

const number = (value: number): string =>
  value.toLocaleString(language, { maximumFractionDigits: 1 });

// How far along its slider a step sits, 0 at the first and 1 at the last.
function at(step: number, steps: readonly number[]): string {
  return String(steps.length > 1 ? step / (steps.length - 1) : 0);
}

/** A duration as the panel says it: ms under a second, s, or min. */
function duration(ms: number): string {
  if (ms >= 60_000)
    return fill(language, 'panel.watch.minutes', { n: number(ms / 60_000) });
  if (ms >= 1_000)
    return fill(language, 'panel.watch.seconds', { n: number(ms / 1_000) });
  return fill(language, 'panel.watch.ms', { n: number(ms) });
}

/** A tolerance step as the panel says it: any change, or a share. */
function share(permille: number): string {
  if (permille === 0) return text(language, 'panel.watch.share.any');
  return fill(language, 'panel.watch.share.percent', {
    n: number(permille / 10),
  });
}

/** A sampling step as the panel says it. */
function rate(ms: number): string {
  if (ms === 0) return text(language, 'panel.watch.sample.realtime');
  if (ms >= 1_000)
    return fill(language, 'panel.watch.every_seconds', {
      n: number(ms / 1_000),
    });
  return fill(language, 'panel.watch.per_second', { n: number(1_000 / ms) });
}

function setHeld(held: boolean): void {
  void invoke('panel_set_held', { held }).catch(() => {});
  done();
}

// The menu's other rows (roadmap 1.60, second slice): each calls the command
// that runs what the matching menu row runs.
function set(command: string, args: Record<string, unknown>): void {
  void invoke(command, args).catch(() => {});
  done();
}

const notifyLabel: Record<PanelNotify, TextKey> = {
  never: 'panel.watch.notify.never',
  once: 'panel.watch.notify.once',
  each: 'panel.watch.notify.each',
};

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
    endEditing();
    (event.currentTarget as HTMLInputElement).blur();
    done();
  }
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') {
    event.preventDefault();
    if (editing) endEditing();
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

<!-- Any press in the panel's window but in the name field hands the keyboard
     back to placing: a label, the header, the padding, the shadow margin or a
     greyed-out switch activate the window as a control does (review of #161,
     round 2). -->
<svelte:window
  onkeydown={onKeydown}
  onpointerup={(event) => {
    if (!(event.target instanceof HTMLInputElement)) done();
  }}
/>

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
        oninput={saveName}
        onblur={endEditing}
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

    {#if view.tuning}
      {@const tuning = view.tuning}
      <label class="field">
        <span class="label"
          >{text(language, 'panel.watch.share')}
          <span class="value">{share(tuning.share_permille)}</span></span
        >
        <input
          type="range"
          min="0"
          max={SHARE_STEPS_PERMILLE.length - 1}
          step="1"
          value={nearestStep(SHARE_STEPS_PERMILLE, tuning.share_permille)}
          style:--at={at(
            nearestStep(SHARE_STEPS_PERMILLE, tuning.share_permille),
            SHARE_STEPS_PERMILLE,
          )}
          oninput={(event) =>
            setTuning({
              share_permille:
                SHARE_STEPS_PERMILLE[Number(event.currentTarget.value)] ?? 0,
            })}
          onchange={done}
        />
      </label>
      <div class="field">
        <span class="label">{text(language, 'panel.watch.notify')}</span>
        <div class="choice" role="radiogroup">
          {#each NOTIFY as notify}
            <button
              type="button"
              role="radio"
              aria-checked={tuning.notify === notify}
              class:on={tuning.notify === notify}
              onclick={() => {
                setTuning({ notify });
                done();
              }}>{text(language, notifyLabel[notify])}</button
            >
          {/each}
        </div>
      </div>
      {#if tuning.notify === 'once'}
        <label class="field">
          <span class="label"
            >{text(language, 'panel.watch.calm')}
            <span class="value">{duration(tuning.calm_ms)}</span></span
          >
          <input
            type="range"
            min="0"
            max={CALM_STEPS_MS.length - 1}
            step="1"
            value={nearestStep(CALM_STEPS_MS, tuning.calm_ms)}
            style:--at={at(nearestStep(CALM_STEPS_MS, tuning.calm_ms), CALM_STEPS_MS)}
            oninput={(event) =>
              setTuning({
                calm_ms: CALM_STEPS_MS[Number(event.currentTarget.value)] ?? 150,
              })}
            onchange={done}
          />
        </label>
      {/if}
      <div class="field">
        <span class="label">{text(language, 'panel.watch.gap')}</span>
        <div class="choice row" role="radiogroup">
          {#each GAPS_MS as gap}
            <button
              type="button"
              role="radio"
              aria-checked={tuning.gap_ms === gap}
              class:on={tuning.gap_ms === gap}
              onclick={() => {
                setTuning({ gap_ms: gap });
                done();
              }}
              >{gap === 0
                ? text(language, 'panel.watch.gap.none')
                : duration(gap)}</button
            >
          {/each}
        </div>
      </div>
      <label class="field">
        <span class="label"
          >{text(language, 'panel.watch.sample')}
          <span class="value">{rate(tuning.sample_ms)}</span></span
        >
        <input
          type="range"
          min="0"
          max={SAMPLE_STEPS_MS.length - 1}
          step="1"
          value={nearestStep(SAMPLE_STEPS_MS, tuning.sample_ms)}
          style:--at={at(nearestStep(SAMPLE_STEPS_MS, tuning.sample_ms), SAMPLE_STEPS_MS)}
          oninput={(event) =>
            setTuning({
              sample_ms: SAMPLE_STEPS_MS[Number(event.currentTarget.value)] ?? 0,
            })}
          onchange={done}
        />
      </label>
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
/* Drawn here rather than left to the system control, whose thumb stops
   short of both ends: at the lowest step the thumb sits flush with the
   start, at the highest flush with the end (his rig pass, 2026-11-07).
   --at is the step as a share of the way, 0 to 1; the fill ends under
   the thumb's centre. */
input[type='range'] {
  appearance: none;
  -webkit-appearance: none;
  display: block;
  width: 100%;
  height: 16px;
  margin: 2px 0 0;
  padding: 0;
  border: 0;
  border-radius: 0;
  background: transparent;
  cursor: pointer;
}

input[type='range']::-webkit-slider-runnable-track {
  height: 6px;
  border-radius: 3px;
  background: linear-gradient(
    to right,
    rgba(var(--tone), 0.9) calc(8px + (100% - 16px) * var(--at, 0)),
    rgba(235, 238, 240, 0.16) calc(8px + (100% - 16px) * var(--at, 0))
  );
}

input[type='range']::-webkit-slider-thumb {
  -webkit-appearance: none;
  appearance: none;
  width: 16px;
  height: 16px;
  margin-top: -5px;
  border: 0;
  border-radius: 50%;
  background: rgb(var(--tone));
}

input[type='range']:focus-visible::-webkit-slider-thumb {
  box-shadow: 0 0 0 3px rgba(var(--tone), 0.35);
}

.label .value {
  float: right;
  color: rgba(var(--tone), 1);
  font-variant-numeric: tabular-nums;
}

.choice.wrap {
  flex-wrap: wrap;
}

.choice.wrap button {
  flex: 0 1 auto;
}

/* One row however long the words: each button as wide as its text. */
.choice.row button {
  flex: 1 1 auto;
  padding: 5px 4px;
  white-space: nowrap;
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
