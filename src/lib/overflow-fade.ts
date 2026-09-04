/**
 * Marks an element whose content is taller than the element, so a cut-off
 * message can be seen to be cut off.
 *
 * # Why this exists (BACKLOG.md I-353)
 *
 * An OCR area's panel is `pointer-events: none`, like every other piece of area
 * chrome: the overlay is click-through (ADR-0016) and the hook hit-tests
 * Rust-side rectangles, so a DOM element that took the pointer would be one the
 * hook does not know about. That is not negotiable and this file does not
 * touch it.
 *
 * What it does mean is that `overflow: auto` on that panel was a **lie**. It
 * rendered a scrollbar the user could not operate, over content they could not
 * reach. The founder hit exactly this at the rig on 2026-09-03: an OCR error
 * message ran past the bottom of a small area, and all he could report was the
 * first line of it.
 *
 * # What replaces it, and what it deliberately does not do
 *
 * The panel clips (`overflow: hidden`, so no scrollbar promises anything) and
 * this action sets `data-overflowing` when there is more text than fits. The
 * stylesheet turns that into a fade at the bottom edge.
 *
 * **It does not make the text reachable, and it is not pretending to.** The way
 * to read the rest is to resize the area, which the user can already do (1.17a).
 * The defect being fixed is that nothing said there was a rest: a silently
 * truncated message and a complete one looked identical.
 */

/**
 * Whether `scrollHeight` exceeds `clientHeight` by enough to be real.
 *
 * The one-pixel tolerance is not a fudge. Sub-pixel layout means a panel that
 * fits exactly can report a `scrollHeight` a fraction larger than its
 * `clientHeight`, and both are rounded to integers before they reach here, so
 * a strict `>` marks perfectly-fitting text as truncated on some zoom levels
 * and not others. A fade that appears under a single line of "No text found"
 * is worse than no fade: it would train the user to ignore it.
 */
export function overflows(scrollHeight: number, clientHeight: number): boolean {
  return scrollHeight - clientHeight > 1;
}

/**
 * Svelte action. Keeps `data-overflowing` on `node` in step with its content.
 *
 * Watched two ways because there are two ways it changes and neither implies
 * the other: the area is resized (the element's box changes, the text does
 * not) and a new recognition lands (the text changes, the box does not).
 * Observing only the first would leave a fade from a previous, longer message
 * sitting under a short one.
 */
export function overflowFade(node: HTMLElement): { destroy(): void } {
  const update = (): void => {
    // Written as a string attribute rather than a class so the CSS reads
    // `[data-overflowing='true']` and cannot be confused with the status
    // classes (`working`, `problem`) that the same element already carries.
    node.dataset.overflowing = String(
      overflows(node.scrollHeight, node.clientHeight),
    );
  };

  update();

  // `ResizeObserver` and `MutationObserver` are both absent in a bare test
  // environment. Guarded rather than assumed: this action's job is a visual
  // hint, and taking a component down at import time because an observer is
  // missing would trade a missing fade for a blank overlay.
  const resize =
    typeof ResizeObserver === 'undefined'
      ? null
      : new ResizeObserver(() => update());
  resize?.observe(node);

  const mutation =
    typeof MutationObserver === 'undefined'
      ? null
      : new MutationObserver(() => update());
  mutation?.observe(node, {
    childList: true,
    characterData: true,
    subtree: true,
  });

  return {
    destroy(): void {
      resize?.disconnect();
      mutation?.disconnect();
    },
  };
}
