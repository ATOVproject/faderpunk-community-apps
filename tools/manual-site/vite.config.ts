import { defineConfig } from "vite";

// SSR build: entry.tsx is bundled for node, then run once to emit static
// HTML. No client bundle — the catalogue page is pure static output.
//
// `.faderpunk` is a symlink to the faderpunk checkout, created by
// build.sh from $FADERPUNK_DIR. Going through a fixed symlink rather than
// a configurable path keeps the import specifiers (and the @source globs
// in src/styles.css, which can't read env vars) stable.
export default defineConfig({
  base: process.env.SITE_BASE ?? "/",
  build: {
    ssr: "src/entry.tsx",
    outDir: "dist",
    emptyOutDir: true,
    target: "node20",
    rollupOptions: {
      output: { entryFileNames: "entry.js" },
    },
  },
  esbuild: {
    // The manual components are .tsx with type-only imports of
    // @atov/fp-config (a generated package that only exists inside the
    // Configurator's own node_modules). esbuild erases type imports, so
    // the bundle never needs it at runtime.
    jsx: "automatic",
  },
});
