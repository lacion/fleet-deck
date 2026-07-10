import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Builds straight into the daemon's committed static dir. fleetd serves
// GET / and /assets/* from scripts/fleetd/board-dist at runtime (resolved
// relative to http.mjs, so both the source and bundle runs find it).
export default defineConfig({
  plugins: [react()],
  base: '/',
  build: {
    outDir: '../scripts/fleetd/board-dist',
    emptyOutDir: true,
  },
  // `npm run dev` against a locally running fleetd (default port 4711).
  server: {
    proxy: {
      '/state': 'http://127.0.0.1:4711',
      '/health': 'http://127.0.0.1:4711',
      '/mail': 'http://127.0.0.1:4711',
      '/command': 'http://127.0.0.1:4711',
      '/api': 'http://127.0.0.1:4711',
      '/ws': { target: 'ws://127.0.0.1:4711', ws: true },
    },
  },
});
