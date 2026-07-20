import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [sveltekit()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: 'ws',
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching Rust sources and build output.
      //
      // `**/target/**` is not optional and not redundant: the Cargo build
      // directory sits at the repo root (workspace layout, architecture.md
      // §2), so it is NOT covered by the `src-tauri` pattern the Tauri
      // scaffold ships. Without it Vite watches `target/debug/deps/*.dll`,
      // which cargo holds a link lock on, and `tauri dev` dies at startup
      // with `EBUSY: resource busy or locked`. Only `tauri dev` is affected —
      // `tauri build` runs no watcher, so CI cannot catch a regression here.
      ignored: ['**/src-tauri/**', '**/target/**'],
    },
  },

  // Vitest. Frontend coverage target is 60% (SPECS/quality-bars.md §2) —
  // component tests, not heavy E2E.
  test: {
    include: ['src/**/*.{test,spec}.{js,ts}'],
  },
}));
