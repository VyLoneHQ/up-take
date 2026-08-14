/**
 * Vite's `?raw` suffix, which imports a file's text instead of its exports.
 *
 * Declared here because this project does not pull in `vite/client`'s ambient
 * types, and `svelte-check` has no other way to know what a `?raw` import is.
 * Four lines against a `@types/node` devDependency: the only consumer is
 * `area-kinds.test.ts`, which reads Rust source to check that the two halves of
 * the wire vocabulary still agree (UP-TAKE `I-55`).
 */
declare module '*?raw' {
  const content: string;
  export default content;
}
