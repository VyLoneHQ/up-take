/**
 * The area-type colour legend, as numbers: one place for each type's colour,
 * read by the notification cards (ADR-0054, the founder 2026-11-03: *"The
 * notification color should also be dependent on the area-type color-legend"*).
 *
 * The values are the legend's own, from the tour's swatches and the areas'
 * borders (`Coach.svelte`, `+page.svelte`). Those two still write their CSS
 * out by hand; moving them onto this table is a follow-up, and
 * `tones.test.ts` checks the swatches against it so the copies cannot drift.
 */

/** A type's colour in the legend. `TypeTone` in `coach-copy.ts` names them. */
export type Tone = 'accent' | 'filter' | 'upscale' | 'text' | 'watch';

/** Each tone as `r, g, b`, for `rgba(var(--tone), alpha)`. */
export const TONES: Record<Tone, string> = {
  accent: '120, 180, 255',
  filter: '255, 186, 110',
  upscale: '180, 200, 235',
  text: '150, 220, 190',
  watch: '120, 225, 170',
};

/** Whether `value` names a tone; anything else is drawn as `accent`. */
export function isTone(value: unknown): value is Tone {
  return typeof value === 'string' && value in TONES;
}

/**
 * The legend colour of an area type, by its wire name (`overlay.rs`
 * `type_name`): the settings panel and the cards are drawn in it.
 */
export function toneOf(kind: string): Tone {
  switch (kind) {
    case 'watch':
      return 'watch';
    case 'filter':
      return 'filter';
    case 'upscale':
      return 'upscale';
    case 'ocr':
      return 'text';
    default:
      return 'accent';
  }
}
