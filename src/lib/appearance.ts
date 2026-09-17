/**
 * The two appearance settings, as CSS custom properties (roadmap 1.14).
 *
 * `UI-UX.md` section 4 gives them as percentages with a default each: an area
 * is 40 % solid and a Filter area's wash is 16 % strong. The stylesheet needs
 * alpha values, and there are five of them per setting (border, fill, glow, and
 * the hovered fill and glow). So the arithmetic lives here, in one function
 * with a test, rather than five times in a `calc()` nobody can check.
 *
 * # The default must be a no-op, and that is what the reference is for
 *
 * The alphas the overlay shipped with are the approved look. A settings row
 * that changed it at its own default would be a redesign smuggled in as a
 * feature, so the scale is anchored: at [`OPACITY_REFERENCE`] the multiplier is
 * exactly 1 and every alpha is the number that was already in the stylesheet.
 * `appearance.test.ts` asserts that, which is the only assertion here that
 * would catch a redesign.
 *
 * # Why a multiplier for one and a direct alpha for the other
 *
 * They are different questions. *How solid an area looks* scales five related
 * alphas together, so it is one factor applied to all of them. *How strong a
 * Filter area is* names the wash's own alpha directly -- `.area.filter`'s fill
 * was `0.16` and the setting's default is 16 -- so a second, invented scale
 * would only be a way to get that correspondence wrong.
 */

/** The percentage at which an area looks exactly as it shipped. */
export const OPACITY_REFERENCE = 40;

/**
 * What the overlay's stylesheet uses. Set on the overlay's root element, so
 * every `.area` inherits them.
 */
export interface AppearanceVars {
  /** Multiplies every accent alpha. `1` is the shipped look. */
  '--area-solidity': string;
  /** A Filter area's wash alpha, `0` to `1`. */
  '--filter-alpha': string;
}

/**
 * The custom properties for a pair of percentages.
 *
 * Out-of-range input is not guarded here: `settings.rs` clamps on read and on
 * write, so a value that reaches this function has already been through the
 * one place that decides what the range is. Clamping again would be a second
 * opinion about the range, and two of those is how they drift apart.
 */
export function appearanceVars(
  areaOpacityPercent: number,
  filterStrengthPercent: number,
): AppearanceVars {
  return {
    '--area-solidity': String(areaOpacityPercent / OPACITY_REFERENCE),
    '--filter-alpha': String(filterStrengthPercent / 100),
  };
}

/** The same, as a `style` attribute. */
export function appearanceStyle(
  areaOpacityPercent: number,
  filterStrengthPercent: number,
): string {
  const vars = appearanceVars(areaOpacityPercent, filterStrengthPercent);
  return Object.entries(vars)
    .map(([name, value]) => `${name}: ${value}`)
    .join('; ');
}
