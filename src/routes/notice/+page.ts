/**
 * The notification card's route (ADR-0054), prerendered for the same reason
 * as the licence window's (see `src/routes/licence/+page.ts`): a second window
 * gets a page file of its own rather than resting on the fallback.
 * `trailingSlash = 'always'` pairs with `notice.rs`'s `WebviewUrl::App("notice/")`.
 */
export const prerender = true;
export const trailingSlash = 'always';
