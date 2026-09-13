<script lang="ts">
import {
  BUTTONS,
  DRAW,
  MODES,
  progress,
  REFERENCE,
  TYPES,
} from '$lib/coach-copy';
import {
  type CoachView,
  cssRectToPhys,
  type Origin,
  type PhysRect,
  physRectToCss,
} from '$lib/overlay-state';

// The first-run coach (roadmap 1.18, ADR-0043). Presentation only, like the
// page that hosts it: Rust owns the tour and says which step to draw and on
// which monitor. The one thing this component tells Rust is where it drew the
// panel and its buttons, because Rust hit-tests presses against those
// rectangles and only the WebView knows how tall wrapped text is. See
// `src-tauri/src/first_run.rs`.

interface Props {
  coach: CoachView;
  origin: Origin;
  dpr: number;
  report: (
    generation: number,
    panel: PhysRect,
    next: PhysRect,
    skip: PhysRect | null,
  ) => void;
}

let { coach, origin, dpr, report }: Props = $props();

const STEPS = [1, 2, 3, 4];
const LAST = STEPS.length;

let panel: HTMLElement | undefined = $state();
let nextButton: HTMLElement | undefined = $state();
let skipButton: HTMLElement | undefined = $state();

const monitor = $derived(physRectToCss(coach.monitor, origin, dpr));
const wide = $derived(coach.step === LAST);
// The mockups' widths, narrowed on a monitor too small to hold them rather
// than allowed to run off its edge.
const width = $derived(
  monitor ? Math.min(wide ? 640 : 500, Math.max(0, monitor.width - 32)) : 0,
);
const left = $derived(monitor ? monitor.x + (monitor.width - width) / 2 : 0);
// Steps 1 to 3 sit near the top, where a drag is least likely to begin; the
// reference sheet is centred, because by then there is nothing to draw.
const top = $derived(
  monitor ? (wide ? monitor.y + monitor.height / 2 : monitor.y + 96) : 0,
);

function measured(element: HTMLElement): PhysRect | null {
  const box = element.getBoundingClientRect();
  return cssRectToPhys(
    { x: box.left, y: box.top, width: box.width, height: box.height },
    origin,
    dpr,
  );
}

// Re-measures whenever anything the drawn geometry depends on changes, and
// always for a new generation: Rust drops the previous layout at every emit
// and accepts only a report naming the emit it answers.
$effect(() => {
  const generation = coach.generation;
  void [coach.step, coach.living, left, top, width];
  if (!panel || !nextButton) return;
  const panelRect = measured(panel);
  const nextRect = measured(nextButton);
  if (panelRect === null || nextRect === null) return;
  report(
    generation,
    panelRect,
    nextRect,
    skipButton ? measured(skipButton) : null,
  );
});
</script>

{#if monitor}
  <div
    class="coach"
    class:wide
    bind:this={panel}
    style="left: {left}px; top: {top}px; width: {width}px"
  >
    <div class="progress">
      <div class="dots">
        {#each STEPS as step (step)}
          <span
            class="dot"
            class:done={step < coach.step}
            class:current={step === coach.step}
          ></span>
        {/each}
      </div>
      <span class="count">{progress(coach.step, LAST)}</span>
    </div>

    {#if coach.step === 1}
      <div class="title">{DRAW.title}</div>
      <div class="body">{DRAW.body}</div>
      <div class="prompt">{DRAW.prompt}</div>
    {:else if coach.step === 2}
      <div class="title">{TYPES.title}</div>
      <div class="body">{TYPES.body}</div>
      <div class="types">
        {#each TYPES.rows as row (row.key)}
          <div class="type-row">
            <span class="keycap tone-{row.tone}">{row.key}</span>
            <span class="swatch tone-{row.tone}"></span>
            <span class="type-name">{row.name}</span>
            <span class="type-does">{row.does}</span>
          </div>
        {/each}
      </div>
    {:else if coach.step === 3}
      <div class="title">{MODES.title}</div>
      <div class="body">{MODES.body}</div>
      <div class="chord">
        <span class="chord-keys">
          {#each MODES.chord as key, index (key)}
            {#if index > 0}<span class="plus">+</span>{/if}
            <span class="keycap tone-accent">{key}</span>
          {/each}
        </span>
        <span class="chord-note">{MODES.chordNote}</span>
      </div>
      <div class="modes">
        <div class="mode" class:active={!coach.living}>
          <div class="mode-name">
            <span class="mode-mark placing"></span>{MODES.placing.name}
          </div>
          <div class="mode-does">{MODES.placing.does}</div>
        </div>
        <div class="mode" class:active={coach.living}>
          <div class="mode-name">
            <span class="mode-mark living"></span>{MODES.living.name}
          </div>
          <div class="mode-does">{MODES.living.does}</div>
        </div>
      </div>
    {:else}
      <div class="title">{REFERENCE.title}</div>
      <div class="body">{REFERENCE.body}</div>
      <div class="sheet">
        <div class="sheet-column">
          <div class="sheet-heading">{REFERENCE.anywhereHeading}</div>
          {#each REFERENCE.anywhere as row (row.keys)}
            <div class="sheet-row">
              <span class="sheet-keys">{row.keys}</span>
              <span class="sheet-does">{row.does}</span>
            </div>
          {/each}
        </div>
        <div class="sheet-column">
          <div class="sheet-heading">{REFERENCE.placingHeading}</div>
          {#each REFERENCE.placing as row (row.keys)}
            <div class="sheet-row">
              <span class="sheet-keys">{row.keys}</span>
              <span class="sheet-does">{row.does}</span>
            </div>
          {/each}
        </div>
      </div>
    {/if}

    <div class="footer">
      <div class="footer-note">
        {#if coach.step === 1}
          <span class="keycap small">{DRAW.escKey}</span>
          <span>{DRAW.escHint}</span>
        {:else if coach.step === 2}
          {TYPES.footer}
        {:else if coach.step === 3}
          {coach.living ? MODES.footerLiving : MODES.footerPlacing}
        {/if}
      </div>
      <div class="actions">
        {#if coach.step < LAST}
          <span class="skip" bind:this={skipButton}>{BUTTONS.skip}</span>
          <span class="next" bind:this={nextButton}>
            {BUTTONS.next}
            <svg
              width="13"
              height="13"
              viewBox="0 0 24 24"
              fill="none"
              stroke="#cfe3ff"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"><path d="M9 6l6 6-6 6"></path></svg
            >
          </span>
        {:else}
          <span class="next finish" bind:this={nextButton}>{BUTTONS.finish}</span>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
/* The area menu's vocabulary at a larger size (UI-UX.md section 3.1): the same
   panel fill, border, radius and shadow as `.menu`. Every value below is the
   approved mockups'. Nothing here takes pointer events: the overlay is
   click-through, and the hook hit-tests the rectangles this component
   reports. */
.coach {
  position: absolute;
  box-sizing: border-box;
  padding: 20px 22px 16px;
  background: rgba(24, 28, 36, 0.96);
  border: 1px solid rgba(120, 180, 255, 0.45);
  border-radius: 6px;
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.5);
  font-family: system-ui, sans-serif;
  color: #ebf0fa;
  pointer-events: none;
}

.coach.wide {
  padding: 26px 28px 20px;
  transform: translateY(-50%);
}

.progress {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 14px;
}

.dots {
  display: flex;
  gap: 4px;
}

.dot {
  width: 18px;
  height: 3px;
  border-radius: 2px;
  background: rgba(120, 180, 255, 0.22);
}

.dot.done {
  background: rgba(120, 180, 255, 0.35);
}

.dot.current {
  background: #78b4ff;
}

.count {
  font-size: 11px;
  color: #7d8496;
  letter-spacing: 0.04em;
}

.title {
  font-size: 19px;
  line-height: 1.3;
  color: #f4f6fa;
  margin-bottom: 10px;
}

.wide .title {
  font-size: 21px;
  margin-bottom: 9px;
}

.body {
  font-size: 13px;
  line-height: 1.55;
  color: rgba(235, 240, 250, 0.72);
  margin-bottom: 16px;
}

.prompt {
  padding: 10px 12px;
  border-radius: 4px;
  background: rgba(120, 180, 255, 0.07);
  border: 1px solid rgba(120, 180, 255, 0.18);
  margin-bottom: 16px;
  font-size: 12px;
  line-height: 1.45;
  color: rgba(235, 240, 250, 0.82);
}

.keycap {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 26px;
  height: 24px;
  padding: 0 6px;
  box-sizing: border-box;
  border-radius: 3px;
  font-size: 12px;
}

.keycap.small {
  min-width: 30px;
  height: 20px;
  font-size: 11px;
  border: 1px solid rgba(235, 240, 250, 0.22);
  background: rgba(235, 240, 250, 0.05);
  color: rgba(235, 240, 250, 0.8);
}

.types {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin-bottom: 18px;
}

.type-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 9px 10px;
  border-radius: 4px;
}

.swatch {
  width: 11px;
  height: 11px;
  flex-shrink: 0;
  box-sizing: border-box;
  border-radius: 2px;
}

.type-name {
  width: 92px;
  font-size: 13px;
  color: #ebf0fa;
}

.type-does {
  font-size: 12px;
  color: rgba(235, 240, 250, 0.6);
}

/* Upscale and Text have no colour of their own yet (UI-UX.md section 5.1).
   These are the mockups' placeholders and choose nothing. */
.keycap.tone-accent {
  border: 1px solid rgba(120, 180, 255, 0.5);
  background: rgba(120, 180, 255, 0.12);
  color: #cfe3ff;
}

.swatch.tone-accent {
  border: 1.5px solid rgba(120, 180, 255, 0.9);
  background: rgba(120, 180, 255, 0.1);
}

.keycap.tone-filter {
  border: 1px solid rgba(255, 186, 110, 0.55);
  background: rgba(255, 170, 80, 0.14);
  color: #ffcd91;
}

.swatch.tone-filter {
  border: 1.5px solid rgba(255, 186, 110, 0.85);
  background: rgba(255, 170, 80, 0.2);
}

.keycap.tone-upscale,
.keycap.tone-text {
  border: 1px solid rgba(235, 240, 250, 0.22);
  background: rgba(235, 240, 250, 0.05);
  color: rgba(235, 240, 250, 0.8);
}

.swatch.tone-upscale {
  border: 1.5px solid rgba(180, 200, 235, 0.75);
  background: rgba(180, 200, 235, 0.12);
}

.swatch.tone-text {
  border: 1.5px solid rgba(150, 220, 190, 0.75);
  background: rgba(150, 220, 190, 0.12);
}

.chord {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px;
  border-radius: 4px;
  background: rgba(120, 180, 255, 0.09);
  border: 1px solid rgba(120, 180, 255, 0.28);
  margin-bottom: 14px;
}

.chord-keys {
  display: flex;
  align-items: center;
  gap: 5px;
  flex-shrink: 0;
}

.chord-keys .keycap {
  height: 26px;
  padding: 0 9px;
}

.plus {
  color: #5d6475;
  font-size: 12px;
}

.chord-note {
  font-size: 12px;
  line-height: 1.45;
  color: rgba(235, 240, 250, 0.82);
}

.modes {
  display: flex;
  gap: 10px;
  margin-bottom: 16px;
}

.mode {
  flex: 1;
  padding: 12px 13px;
  border-radius: 4px;
  background: rgba(235, 240, 250, 0.03);
  border: 1px solid rgba(235, 240, 250, 0.09);
}

.mode.active {
  border-color: rgba(120, 180, 255, 0.35);
}

.mode-name {
  display: flex;
  align-items: center;
  gap: 7px;
  margin-bottom: 7px;
  font-size: 12px;
  color: #ebf0fa;
}

.mode-mark {
  width: 8px;
  height: 8px;
  border-radius: 2px;
}

.mode-mark.placing {
  background: #78b4ff;
}

.mode-mark.living {
  background: #5d6475;
}

.mode-does {
  font-size: 11px;
  line-height: 1.5;
  color: rgba(235, 240, 250, 0.58);
}

.sheet {
  display: flex;
  gap: 30px;
  margin: 6px 0 22px;
}

.sheet-column {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.sheet-heading {
  font-size: 11px;
  color: #7d8496;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  margin-bottom: 1px;
}

.sheet-row {
  display: flex;
  align-items: baseline;
  gap: 10px;
}

.sheet-keys {
  width: 124px;
  flex-shrink: 0;
  font-family: ui-monospace, 'Cascadia Code', Consolas, monospace;
  font-size: 11.5px;
  color: #cfe3ff;
}

.sheet-does {
  font-size: 12px;
  line-height: 1.4;
  color: rgba(235, 240, 250, 0.66);
}

.footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.footer-note {
  display: flex;
  align-items: center;
  gap: 7px;
  font-size: 12px;
  color: #7d8496;
}

.actions {
  display: flex;
  align-items: center;
  gap: 18px;
  flex-shrink: 0;
}

.skip {
  font-size: 12px;
  color: #7d8496;
}

.next {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 7px 14px;
  border-radius: 4px;
  background: rgba(120, 180, 255, 0.16);
  border: 1px solid rgba(120, 180, 255, 0.5);
  font-size: 13px;
  color: #cfe3ff;
}

.next.finish {
  padding: 8px 18px;
}
</style>
