<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { onMount } from 'svelte';
import { createSaveQueue } from '$lib/save-queue';
import {
  type Facts,
  languageFor,
  type Pane,
  type PaneId,
  panes,
  type Row,
  type Settings,
} from '$lib/settings-model';
import { isLanguage, type Language, text } from '$lib/strings';

// Presentation only, the same rule the overlay page follows (architecture §1):
// `settings-model.ts` decides which rows exist and what each one does to the
// settings object, `settings.rs` decides what a setting means, and this file
// draws. The one judgement made here is when to send -- see `commit` below.

let language = $state<Language>('en');
let settings = $state<Settings | null>(null);
let facts = $state<Facts | null>(null);
let pane = $state<PaneId>('general');
let problem = $state('');
let tourArmed = $state(false);

const view = $derived(
  settings && facts ? panes(language, settings, facts) : [],
);
const showing = $derived(view.find((each) => each.id === pane) ?? view[0]);

/**
 * The one thing that writes. At most one save in flight, newest value wins.
 *
 * ⚠️ **`commit` used to `await invoke` directly**, and a slider's `input`
 * event fires on every pixel of a drag -- so a drag started dozens of
 * independent saves with nothing sequencing them, an older value could land
 * after a newer one, and `config.toml` was rewritten once per pixel. Found by
 * round 2 of `PR #105`'s independent review. `save-queue.ts` carries the
 * argument and the tests that would fail against the old shape.
 */
const save = createSaveQueue<Settings>(
  (next) => invoke('settings_write', { settings: next }),
  (reason) => {
    problem = reason;
  },
  async () => {
    // The registration may have been written or removed by that save, so the
    // fact the General pane shows about this machine is re-read rather than
    // assumed to have followed.
    facts = await invoke<Facts>('settings_facts');
  },
);

/**
 * Shows a change at once and queues it to be saved.
 *
 * **Optimistic, deliberately.** The control shows the new value immediately and
 * Rust is told after, because every one of these settings is in force the
 * moment the store is written and a control that waited for a disk write would
 * lag behind a drag. A save that could not reach the disk still takes effect
 * for this run, which is why the message is *not saved* rather than
 * *not changed*.
 */
function commit(next: Settings): void {
  // The window re-renders in the chosen language at once. The overlay and the
  // native menus cannot: Rust decides its language once per process and hands
  // out `&'static str`, so a live switch there would half-translate the tray
  // until the next restart. The row's own sentence says which is which.
  if (facts) {
    const wanted = languageFor(next.language, facts);
    if (isLanguage(wanted)) language = wanted;
  }
  settings = next;
  problem = '';
  save(next);
}

async function chooseFolder(row: Extract<Row, { shape: 'folder' }>) {
  // The picker is opened Rust-side. `lib.rs` notes that no frontend capability
  // grants the dialog plugin, so the WebView cannot open dialogs -- keeping it
  // that way is worth one command.
  const chosen = await invoke<string | null>('settings_choose_folder');
  if (chosen) commit(row.set(chosen));
}

async function replayTour() {
  await invoke('settings_replay_tour');
  tourArmed = true;
}

onMount(() => {
  void (async () => {
    const chosen = await invoke('overlay_language');
    if (isLanguage(chosen)) language = chosen;
    settings = await invoke<Settings>('settings_read');
    facts = await invoke<Facts>('settings_facts');
    // Shown only now. The window is created hidden so nobody sees an unpainted
    // frame; on a dark panel that flash is bright enough to be the first thing
    // anyone reports.
    await getCurrentWindow().show();
  })();
});
</script>

<svelte:head>
  <title>{text(language, 'settings.title')}</title>
</svelte:head>

<div class="window">
  <!-- A slim title bar of our own (UI-UX.md §3.2), so the window belongs to the
       same product as the overlay rather than to Windows. `data-tauri-drag-region`
       is what makes an undecorated window movable. -->
  <header class="titlebar" data-tauri-drag-region>
    <span class="product" data-tauri-drag-region
      >{text(language, 'settings.title')}</span
    >
    <button
      type="button"
      class="close"
      aria-label={text(language, 'settings.close')}
      onclick={() => invoke('settings_close')}>&times;</button
    >
  </header>

  <div class="body">
    <nav class="sidebar" aria-label={text(language, 'settings.title')}>
      {#each view as each (each.id)}
        <button
          type="button"
          class="tab"
          class:current={each.id === pane}
          aria-current={each.id === pane ? 'page' : undefined}
          onclick={() => {
            pane = each.id;
          }}>{each.name}</button
        >
      {/each}
    </nav>

    <main class="pane">
      {#if showing}
        {#each showing.sections as section, index (section.label + index)}
          {#if section.label}
            <h2 class="section">{section.label}</h2>
          {/if}
          <div class="rows">
            {#each section.rows as row (row.id)}
              <div class="row" class:stacked={row.shape === 'keys'}>
                <div class="what">
                  {#if row.name}<span class="name">{row.name}</span>{/if}
                  {#if row.about}<span class="about">{row.about}</span>{/if}
                  {#if row.shape === 'toggle' && row.warning}
                    <span class="warning">{row.warning}</span>
                  {/if}
                </div>

                <div class="control">
                  {#if row.shape === 'toggle'}
                    <button
                      type="button"
                      role="switch"
                      aria-checked={row.value}
                      aria-label={row.name}
                      class="toggle"
                      class:on={row.value}
                      onclick={() => commit(row.set(!row.value))}
                    >
                      <span class="knob"></span>
                    </button>
                  {:else if row.shape === 'segmented'}
                    <div class="segmented" role="group" aria-label={row.name}>
                      {#each row.segments as segment (segment.value)}
                        <button
                          type="button"
                          class="segment"
                          class:on={segment.value === row.value}
                          aria-pressed={segment.value === row.value}
                          onclick={() => commit(row.set(segment.value))}
                          >{segment.label}</button
                        >
                      {/each}
                    </div>
                  {:else if row.shape === 'slider'}
                    <label class="slider">
                      <input
                        type="range"
                        min={row.min}
                        max={row.max}
                        value={row.value}
                        aria-label={row.name}
                        oninput={(event) =>
                          commit(row.set(event.currentTarget.valueAsNumber))}
                      />
                      <span class="reading">{row.value}%</span>
                    </label>
                  {:else if row.shape === 'folder'}
                    <div class="folder">
                      <span class="path" class:default={!row.value}
                        >{row.value || row.placeholder}</span
                      >
                      <button
                        type="button"
                        class="quiet"
                        onclick={() => chooseFolder(row)}>{row.choose}</button
                      >
                      {#if row.value}
                        <button
                          type="button"
                          class="quiet"
                          onclick={() => commit(row.set(null))}
                          >{row.reset}</button
                        >
                      {/if}
                    </div>
                  {:else if row.shape === 'fact'}
                    <kbd class="fact">{row.value}</kbd>
                  {:else if row.shape === 'action'}
                    <button
                      type="button"
                      class="quiet"
                      disabled={tourArmed}
                      onclick={replayTour}
                      >{tourArmed
                        ? text(language, 'settings.help.replay.armed')
                        : row.label}</button
                    >
                  {:else if row.shape === 'keys'}
                    <!-- Drawn by the stacked branch below. -->
                  {/if}
                </div>

                {#if row.shape === 'keys'}
                  <h3 class="keys-heading">{row.heading}</h3>
                  <dl class="keys">
                    {#each row.keys as key (key.does)}
                      <dt><kbd>{key.keys}</kbd></dt>
                      <dd>{key.does}</dd>
                    {/each}
                  </dl>
                {/if}
              </div>
            {/each}
          </div>
        {/each}
      {/if}

      {#if problem}
        <p class="problem" role="status">
          {text(language, 'settings.saved_but_not_stored').replace(
            '{reason}',
            problem,
          )}
        </p>
      {/if}
    </main>
  </div>
</div>

<style>
/* The overlay's palette (UI-UX.md §2), which is `+page.svelte`'s `<style>`
   block. Those values are the source of truth; these are the same ones, named
   here as custom properties because this window uses each of them in several
   places and the overlay uses them once each. */
:global(html),
:global(body) {
  margin: 0;
  height: 100%;
  /* Transparent, so the panel's own rounded corners are what shows rather
     than a dark rectangle behind them. */
  background: transparent;
}

.window {
  --accent: rgba(120, 180, 255, 0.9);
  --accent-bright: rgba(160, 210, 255, 1);
  --panel: rgba(24, 28, 36, 0.96);
  --deep: rgb(12, 14, 18);
  --text: rgba(235, 240, 250, 0.95);
  --muted: rgba(235, 240, 250, 0.55);
  --hairline: rgba(235, 240, 250, 0.08);

  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  height: 100vh;
  overflow: hidden;
  border-radius: 6px;
  background: var(--panel);
  color: var(--text);
  /* 13px/1.4, not /1: at `1` the descenders are clipped, found on the rig
     2026-08-25 and recorded in UI-UX.md §2. */
  font: 13px/1.4 system-ui, sans-serif;
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.5);
}

.titlebar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex: none;
  height: 34px;
  padding: 0 6px 0 12px;
  border-bottom: 1px solid var(--hairline);
  background: var(--deep);
}

.product {
  font-size: 12px;
  letter-spacing: 0.02em;
  color: var(--muted);
  /* The bar is the drag region, so the label must not swallow a drag. */
  pointer-events: none;
}

.close {
  width: 28px;
  height: 22px;
  border: 0;
  border-radius: 3px;
  background: transparent;
  color: var(--muted);
  font-size: 16px;
  line-height: 1;
  cursor: pointer;
}
.close:hover {
  background: rgba(235, 240, 250, 0.1);
  color: var(--text);
}

.body {
  display: flex;
  flex: 1;
  min-height: 0;
}

.sidebar {
  display: flex;
  flex-direction: column;
  gap: 2px;
  flex: none;
  /* 196px (UI-UX.md §3.2). */
  width: 196px;
  padding: 12px 8px;
  border-right: 1px solid var(--hairline);
}

.tab {
  padding: 7px 10px;
  border: 0;
  border-radius: 4px;
  background: transparent;
  color: var(--muted);
  font: inherit;
  text-align: left;
  cursor: pointer;
}
.tab:hover {
  color: var(--text);
  background: rgba(235, 240, 250, 0.05);
}
.tab.current {
  color: var(--accent-bright);
  background: rgba(120, 180, 255, 0.12);
}

.pane {
  flex: 1;
  min-width: 0;
  padding: 16px 22px 24px;
  overflow-y: auto;
}

.section {
  margin: 18px 0 2px;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
}
.section:first-child {
  margin-top: 2px;
}

.row {
  display: flex;
  align-items: flex-start;
  gap: 24px;
  padding: 13px 0;
  border-bottom: 1px solid var(--hairline);
}
.row:last-child {
  border-bottom: 0;
}
.row.stacked {
  flex-wrap: wrap;
  gap: 6px;
}

.what {
  display: flex;
  flex-direction: column;
  gap: 3px;
  flex: 1;
  min-width: 0;
}

.name {
  color: var(--text);
}

.about {
  color: var(--muted);
  font-size: 12px;
}

.warning {
  /* Amber, never red: nothing is broken and no data is lost (UI-UX.md §2). */
  color: #f3c969;
  font-size: 12px;
}

.control {
  display: flex;
  align-items: center;
  flex: none;
}

/* --- the three shapes ---------------------------------------------------- */

.toggle {
  width: 38px;
  height: 22px;
  padding: 2px;
  border: 1px solid var(--hairline);
  border-radius: 11px;
  background: rgba(235, 240, 250, 0.08);
  cursor: pointer;
  transition: background 120ms ease;
}
.toggle.on {
  background: var(--accent);
  border-color: var(--accent);
}
.knob {
  display: block;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: var(--text);
  transition: transform 120ms ease;
}
.toggle.on .knob {
  transform: translateX(16px);
  background: var(--deep);
}

.segmented {
  display: flex;
  border: 1px solid var(--hairline);
  border-radius: 4px;
  overflow: hidden;
}
.segment {
  padding: 5px 11px;
  border: 0;
  background: transparent;
  color: var(--muted);
  font: inherit;
  white-space: nowrap;
  cursor: pointer;
}
.segment + .segment {
  border-left: 1px solid var(--hairline);
}
.segment:hover {
  color: var(--text);
}
.segment.on {
  background: rgba(120, 180, 255, 0.18);
  color: var(--accent-bright);
}

.slider {
  display: flex;
  align-items: center;
  gap: 10px;
}
.slider input {
  width: 160px;
  accent-color: #78b4ff;
}
.reading {
  width: 34px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
  text-align: right;
}

/* --- the rows that are not settings -------------------------------------- */

.folder {
  display: flex;
  align-items: center;
  gap: 8px;
  max-width: 420px;
}
.path {
  max-width: 230px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  direction: rtl;
  text-align: left;
  color: var(--text);
  font-size: 12px;
}
.path.default {
  color: var(--muted);
}

.quiet {
  padding: 5px 10px;
  border: 1px solid var(--hairline);
  border-radius: 4px;
  background: transparent;
  color: var(--text);
  font: inherit;
  white-space: nowrap;
  cursor: pointer;
}
.quiet:hover:not(:disabled) {
  border-color: var(--accent);
  color: var(--accent-bright);
}
.quiet:disabled {
  color: var(--muted);
  cursor: default;
}

kbd {
  padding: 2px 6px;
  border: 1px solid var(--hairline);
  border-radius: 3px;
  background: rgba(235, 240, 250, 0.05);
  font: inherit;
  font-size: 12px;
  white-space: nowrap;
}

.keys-heading {
  width: 100%;
  margin: 8px 0 0;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
}
.keys {
  display: grid;
  grid-template-columns: max-content 1fr;
  gap: 6px 14px;
  width: 100%;
  margin: 4px 0 0;
}
.keys dt {
  margin: 0;
}
.keys dd {
  margin: 0;
  color: var(--muted);
}

.problem {
  margin: 18px 0 0;
  padding: 10px 12px;
  border: 1px solid rgba(243, 201, 105, 0.35);
  border-radius: 4px;
  background: rgba(243, 201, 105, 0.08);
  color: #f3c969;
  font-size: 12px;
}
</style>
