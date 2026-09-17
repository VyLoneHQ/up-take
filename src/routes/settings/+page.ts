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
 * It is wrong for a second window. Tauri serves `frontendDist` as static files
 * and does not fall back to `index.html` for an unknown path, so a window
 * opened at `settings/` in a release build would get a 404 where its page
 * should be -- and **only** in a release build, because `pnpm dev` is a real
 * dev server and does fall back. That is the worst shape a defect can have
 * here: invisible in the loop this is developed in.
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
