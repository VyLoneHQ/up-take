/**
 * The area settings panel's route (roadmap 1.60), prerendered for the same
 * reason as the licence window's (see `src/routes/licence/+page.ts`).
 * `trailingSlash = 'always'` pairs with `panel.rs`'s `WebviewUrl::App("panel/")`.
 */
export const prerender = true;
export const trailingSlash = 'always';
