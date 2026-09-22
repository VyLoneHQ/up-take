/**
 * What the overlay page hides while a freeze captures (ADR-0019 decision 6,
 * backlog I-426).
 *
 * A freeze covers the cursor's monitor, or every monitor with the 1.14
 * setting. With *Show UP-TAKE in screen recordings* on, the overlay is in
 * every capture including UP-TAKE's own, so for the length of the capture the
 * page hides everything it draws on the covered monitors, and only those: the
 * other monitors keep their areas on screen.
 *
 * It is done with one `clip-path` on the page's root rather than by hiding
 * elements one by one, because the rule is "nothing drawn inside these
 * rectangles", and a per-element rule would have to be remembered by every
 * element added later. A clip on the root cannot be forgotten by a child.
 */
import type { CssRect } from '$lib/regions';

/**
 * Far enough outside any desktop that the outer ring always contains the
 * whole page. The clip is in the page's own CSS pixels, and no monitor
 * arrangement comes near a million of them.
 */
const FAR = 1_000_000;

/**
 * The `clip-path` that keeps everything on the page except `hidden`, or null
 * when nothing is hidden.
 *
 * One outer ring around the whole page, with one hole per hidden rectangle,
 * under the even-odd rule. **Holes must not overlap**, because under even-odd
 * an overlap is filled again, and so is it under non-zero. Monitors never
 * overlap on Windows, and adjacent ones only share an edge, which is tested.
 */
export function hiddenClipPath(hidden: readonly CssRect[]): string | null {
  if (hidden.length === 0) return null;
  const outer = `M${-FAR} ${-FAR}H${FAR}V${FAR}H${-FAR}Z`;
  const holes = hidden
    .map(
      (rect) =>
        `M${rect.x} ${rect.y}h${rect.width}v${rect.height}h${-rect.width}Z`,
    )
    .join('');
  return `path(evenodd, '${outer}${holes}')`;
}

/** Resolves after the browser has painted the next frame. */
export function afterNextPaint(): Promise<void> {
  return new Promise((resolve) => {
    requestAnimationFrame(() => resolve());
  });
}

/** What `overlay://freeze-hide` carries: the token, and the covered monitors. */
export interface FreezeHidePayload {
  token: number;
  rects: [x: number, y: number, width: number, height: number][];
}

/** What `overlay://freeze-reveal` carries. */
export interface FreezeRevealPayload {
  token: number;
}
