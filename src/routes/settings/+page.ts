/**
 * The settings window's route (roadmap 1.14).
 *
 * # Why this file exists at all
 *
 * `svelte.config.js` uses `adapter-static` with `fallback: 'index.html'`, which
 * is SPA mode: **nothing is prerendered**, one `index.html` is emitted, and the
 * client router resolves every path after the page has loaded. That is right
 * for the overlay, which is the only window that existed until now and is
 * always opened at `/`.
 *
 * A second window gets a page file of its own instead. When this was written,
 * the reasoning was that Tauri serves `frontendDist` as static files and does
 * not fall back to `index.html` for an unknown path, so a window opened at
 * `settings/` in a release build would get a 404, and only in a release build,
 * because `pnpm dev` is a real dev server and does fall back. **That is not
 * true of Tauri 2.12**, which tries `{path}.html`, then `{path}/index.html`,
 * then `index.html` (`tauri/src/manager/mod.rs`, read by the review of #134).
 * The prerender is kept so the window loads its own file and nothing rests on
 * that fallback staying.
 *
 * So this route is prerendered. `adapter-static` honours `prerender` even with
 * a fallback set, and emits this page as a file of its own beside the fallback.
 *
 * # `trailingSlash`, which is not a style choice
 *
 * With `'always'` the emitted file is `settings/index.html` rather than
 * `settings.html`, so the request Tauri makes for `settings/` resolves the way
 * a directory index does, and `location.pathname` is then `/settings/` -- which
 * is what this route matches. The pairing is what makes the built window work,
 * and `settings_window.rs`'s `WebviewUrl` is the other half of it.
 */
export const prerender = true;
export const trailingSlash = 'always';
