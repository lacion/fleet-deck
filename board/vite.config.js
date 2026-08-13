import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// The locally running fleetd we proxy `npm run dev` to, in its two schemes.
// FLEETDECK_PORT is the same variable the daemon itself reads (see
// src/daemon/ports.mjs), so one env var points both sides at the same
// scratch daemon. Default 4711 — but when you run a scratch daemon on 4712
// to stay off your real fleet (CONTRIBUTING.md), export FLEETDECK_PORT=4712
// for `npm run dev` too, or the dev board silently reads and mutates the
// REAL fleet on 4711 (mail, commands, API writes, terminal input).
const FLEETD_PORT = process.env.FLEETDECK_PORT || '4711';
const FLEETD_HTTP = `http://127.0.0.1:${FLEETD_PORT}`;
const FLEETD_WS = `ws://127.0.0.1:${FLEETD_PORT}`;
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
// GET / and /assets/* from src/daemon/board-dist at runtime (resolved
// relative to http.mjs, so both the source and bundle runs find it).
export default defineConfig({
  plugins: [react()],
  // RELATIVE, not '/': the board must load under a path-based reverse proxy
  // (Coder serves apps at /@user/ws.agent/apps/<slug>/ and strips that prefix
  // before forwarding, without telling the app it ever existed). Relative asset
  // URLs resolve against the document, so one build works at the root, under any
  // prefix, and behind any proxy. The API/WS side of the same problem is solved
  // at runtime in src/base.js — read that file before changing this line.
  base: './',
  build: {
    outDir: '../src/daemon/board-dist',
    emptyOutDir: true,
  },
  // `npm run dev` against a locally running fleetd (FLEETDECK_PORT, default 4711).
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
    // The board is its own package (board/), but it imports the shared wire
    // contracts from the repo root (../contracts). Vite's dev server refuses to
    // serve files above the project root by default, so widen the allow-list to
    // the repo root. The production `vite build` (Rollup) already follows the
    // import across the boundary without this — it only matters for `npm run dev`.
    fs: { allow: ['..'] },
  },
});
