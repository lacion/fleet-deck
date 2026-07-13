import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// The locally running fleetd we proxy `npm run dev` to (default port 4711),
// in its two schemes.
const FLEETD_HTTP = 'http://127.0.0.1:4711';
const FLEETD_WS = 'ws://127.0.0.1:4711';
// The Origin the daemon's C1 gate accepts is its OWN address. Browsers send an
// Origin of http(s):// even for a WebSocket upgrade, so the http scheme is the
// right value for both HTTP requests AND /ws + /ws/term upgrades.
const FLEETD_ORIGIN = FLEETD_HTTP;

// R1-3 — the C1 gate rejects any request whose Host/Origin isn't the daemon's
// own. In dev the browser talks to Vite (:5173), so its requests carry
// Origin http://127.0.0.1:5173 and Host 127.0.0.1:5173 — both 403 at the gate,
// and the WS upgrades for /ws and /ws/term are refused for the same reason.
//
// changeOrigin rewrites the Host header to the target, but it does NOT touch
// Origin, so we set Origin ourselves on the outgoing HTTP request (proxyReq)
// and on the WebSocket upgrade (proxyReqWs). Together they make the dev board's
// POSTs and sockets look, to the daemon, like they came from the daemon itself.
const rewriteOrigin = (proxy) => {
  proxy.on('proxyReq', (proxyReq) => proxyReq.setHeader('origin', FLEETD_ORIGIN));
  proxy.on('proxyReqWs', (proxyReq) => proxyReq.setHeader('origin', FLEETD_ORIGIN));
};

const httpProxy = { target: FLEETD_HTTP, changeOrigin: true, configure: rewriteOrigin };

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
      '/state': httpProxy,
      '/health': httpProxy,
      '/mail': httpProxy,
      '/command': httpProxy,
      '/api': httpProxy,
      // matches /ws AND /ws/term — both upgrades need the Origin rewrite too.
      '/ws': { target: FLEETD_WS, ws: true, changeOrigin: true, configure: rewriteOrigin },
    },
  },
});
