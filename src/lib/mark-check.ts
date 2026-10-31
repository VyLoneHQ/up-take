import type { CssRect as Box } from './regions';

/**
 * Whether a watching area's mark can be seen (roadmap 1.58, ADR-0048: "visibly
 * marked while it watches").
 *
 * The host reads a watch area only while the page has confirmed its mark
 * (`overlay_confirm_marks`, `src-tauri/src/watch.rs`). ADR-0048's *Revisit if*
 * asks for the mark to be enforced by the reader rather than trusted from the
 * page's intent; the page is the only side that knows where the mark is drawn,
 * how large it is at this scale and in this language, and what is drawn over
 * it, so it measures and the host waits for the answer. Rounds 2 to 19 of the
 * review of #154 found routes past every fixed size the host tried instead.
 *
 * All boxes are CSS pixels in the overlay's own coordinates.
 */
export type { Box };

const contains = (outer: Box, inner: Box): boolean =>
  inner.x >= outer.x &&
  inner.y >= outer.y &&
  inner.x + inner.width <= outer.x + outer.width &&
  inner.y + inner.height <= outer.y + outer.height;

const overlaps = (a: Box, b: Box): boolean =>
  a.x < b.x + b.width &&
  b.x < a.x + a.width &&
  a.y < b.y + b.height &&
  b.y < a.y + a.height;

/**
 * The mark is seen when its dot, the part drawn first and clipped last, is
 * **wholly** inside the badge's clip box (not cut off by a small area), inside
 * one monitor (not off the desktop or in a gap), and under no area drawn above
 * this one. Any overlapping area counts as covering it, opaque or not, which
 * errs on the side of reading less. An empty dot box (nothing laid out) is not
 * seen.
 */
export function markSeen(
  dot: Box,
  badge: Box,
  monitors: readonly Box[],
  above: readonly Box[],
): boolean {
  if (!(dot.width > 0 && dot.height > 0)) return false;
  if (!contains(badge, dot)) return false;
  if (!monitors.some((monitor) => contains(monitor, dot))) return false;
  return !above.some((area) => overlaps(area, dot));
}
