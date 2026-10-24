import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// The deck embeds `dist/` with rust-embed (PLAN §12) and serves it at `/`.
// `base: './'` keeps asset URLs relative, so the board also loads under a
// path prefix of a reverse proxy.
export default defineConfig({
  plugins: [react()],
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
});
