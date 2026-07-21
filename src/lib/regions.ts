/**
 * The overlay's side of the interactive-region contract (roadmap task 1.2,
 * corrected in the 1.3 follow-up).
 *
 * Kept out of the Svelte component so it can be tested without a DOM harness,
 * and so the component stays what architecture §1 asks for: render state, emit
 * intents, hold no logic.
 */

/** A rectangle in CSS pixels, relative to the overlay's viewport. */
export interface CssRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Anything that can report its own box — an `HTMLElement` in the app, a plain
 * stub in tests. Narrower than `HTMLElement` on purpose: this module needs one
 * method, and depending on the whole DOM type would force a jsdom environment
 * for no gain.
 */
export interface Measurable {
  getBoundingClientRect(): {
    x: number;
    y: number;
    width: number;
    height: number;
  };
}

/** The Tauri `invoke` function, injected so tests need no module mocking. */
export type Invoke = (
  command: string,
  args?: Record<string, unknown>,
) => Promise<unknown>;

/**
 * Measures elements into CSS rects, skipping any that are not yet bound.
 *
 * Svelte's `bind:this` is undefined until mount, and a report is triggered by
 * `resize` as well as by mount — so an unbound element is an ordinary race, not
 * an error. Dropping it is right: a missing region costs click-through over
 * that element, while a `null` deref here would abort the whole report and cost
 * click-through everywhere.
 */
export function measure(
  elements: readonly (Measurable | null | undefined)[],
): CssRect[] {
  const rects: CssRect[] = [];
  for (const element of elements) {
    if (!element) continue;
    const { x, y, width, height } = element.getBoundingClientRect();
    rects.push({ x, y, width, height });
  }
  return rects;
}

/**
 * Reports the interactive regions to Rust, together with **the scale factor
 * they were measured in**.
 *
 * Sending the scale is the whole point of this function. Rust used to convert
 * CSS→physical with the scale factor tao reported for the window, on the
 * assumption that it matched the one the WebView laid out in. It does not
 * always: when the overlay window is created on a monitor at one scale and then
 * resized to span a virtual desktop where Windows assigns it another, tao's
 * value and `devicePixelRatio` disagree, and every region lands offset by the
 * ratio between them. Measured on the dev rig: the WebView laid out a 4448 CSS
 * px viewport (1.25) while tao reported 1.0, putting the hint pill 556 px from
 * where it was drawn and making it unclickable.
 *
 * `devicePixelRatio` is by definition the factor the reported rects were
 * measured in, so pairing the two is self-consistent by construction — there is
 * no window for them to drift apart in.
 *
 * Never throws. Returns whether the report landed. A rejected `invoke` that
 * escaped here would be an unhandled rejection, and the Rust side treats "no
 * regions reported" as *keep the whole window interactive*, so a lost report
 * costs click-through and never the dismiss path.
 *
 * **A mount race is not reported.** If elements were passed but every one was
 * unbound, `measure` returns nothing — and sending that empty set would replace
 * a good region set with one meaning *the whole window takes input*, so the
 * overlay would swallow every click across the virtual desktop until some later
 * `resize` happened to re-report. Skipping is strictly better: the previous set
 * survives, and the report that follows the mount replaces it. Reporting
 * genuinely zero regions is still possible — pass no elements at all.
 */
export async function reportInteractiveRegions(
  invoke: Invoke,
  elements: readonly (Measurable | null | undefined)[],
  scale: number,
): Promise<boolean> {
  const regions = measure(elements);
  if (regions.length === 0 && elements.length > 0) return false;
  try {
    await invoke('overlay_set_interactive_regions', { regions, scale });
    return true;
  } catch (error) {
    console.error('Failed to report interactive regions:', error);
    return false;
  }
}

/**
 * Emits the hide intent. Never throws, for the same reason as above: this is
 * one of only two dismiss paths, and an unhandled rejection here would strand
 * the user behind a full-desktop overlay.
 */
export async function hideOverlay(invoke: Invoke): Promise<boolean> {
  try {
    await invoke('overlay_hide');
    return true;
  } catch (error) {
    console.error('Failed to hide the overlay:', error);
    return false;
  }
}

/** Whether a key press should dismiss the overlay (M-11 keyboard-only). */
export function isDismissKey(key: string): boolean {
  return key === 'Escape';
}
