/**
 * The licence window's route (I-443), prerendered for the same reason as the
 * settings window's: a second window gets a page file of its own rather than
 * resting on the fallback. See `src/routes/settings/+page.ts`.
 *
 * Tauri 2.12 does fall back to `index.html` for an unknown path (a review of
 * #134 read it in `tauri/src/manager/mod.rs`), so the settings file's premise
 * that it does not is no longer true of the version in use. Prerendering is
 * kept anyway: the window then loads its own file, and nothing depends on that
 * fallback staying.
 *
 * `trailingSlash = 'always'` pairs with `licence_window.rs`'s
 * `WebviewUrl::App("licence/?which=...")`. The page reads the query from
 * `window.location` after it mounts, never while it is prerendered.
 */
export const prerender = true;
export const trailingSlash = 'always';
