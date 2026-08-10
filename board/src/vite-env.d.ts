// Ambient types for the Vite-bundled board.
//
// `import.meta.env` is Vite's build-constant bag. It is declared OPTIONAL here on
// purpose: base.ts guards it with `?.` because the pure-ESM modules (base / qr /
// util / staleChunks) also load under `node --test` with no bundler, where Vite
// never defines it. A non-optional `vite/client` typing would make that guard a
// lint error, so the board carries this minimal, honest declaration instead.
interface ImportMetaEnv {
  readonly DEV?: boolean;
}

interface ImportMeta {
  readonly env?: ImportMetaEnv;
}
