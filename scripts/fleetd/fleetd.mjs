#!/usr/bin/env node
// fleetd — Fleet Deck daemon (Phase 1: daemon parity).
// One process, one port, 127.0.0.1 only. State lives in SQLite
// (FLEETDECK_HOME/fleetd.db, WAL) so it survives daemon restarts.
// Election: whoever binds the port is the server; EADDRINUSE losers exit 3.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from './db.mjs';
import { createCore } from './derive.mjs';
import { createHttp } from './http.mjs';
import { startAgentsPoll } from './agents-poll.mjs';
import { createPayloadCapture } from './payload-capture.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.FLEETDECK_PORT || 4711);
const HOME = process.env.FLEETDECK_HOME || path.join(os.homedir() || '/tmp', '.fleetdeck');
fs.mkdirSync(HOME, { recursive: true });

let version = '0.0.0';
try {
  version = JSON.parse(fs.readFileSync(path.resolve(__dirname, '../../package.json'), 'utf8')).version || version;
} catch { /* standalone install; /health just reports 0.0.0 */ }

const db = openDb(path.join(HOME, 'fleetd.db'));
const core = createCore(db, { port: PORT }); // holdMs resolves from FLEETDECK_HOLD_MS inside
const { server } = createHttp(core, {
  port: PORT,
  // the Phase 1 spike board, kept verbatim at GET /plain; the real board
  // (GET / + /assets/*) is served from board-dist, resolved inside http.mjs
  boardFile: path.join(__dirname, 'board.html'),
  version,
  // validation aid: first 3 raw payloads per hook event → HOME/hook-payloads.jsonl
  capture: createPayloadCapture(HOME),
});

// -------------------------------------------------------------- election
server.on('error', (e) => {
  if (e.code === 'EADDRINUSE') {
    console.error('fleetd already running (port bind lost the election)');
    process.exit(3);
  }
  throw e;
});

const PID_FILE = path.join(HOME, 'fleetd.pid');
server.listen(PORT, '127.0.0.1', () => {
  try { fs.writeFileSync(PID_FILE, String(process.pid)); } catch { /* home not writable */ }
  console.log(`fleetd up on http://127.0.0.1:${PORT} (pid ${process.pid}, db ${path.join(HOME, 'fleetd.db')})`);
  // v1.2 restart reconciliation: spawn rows survive in SQLite, panes survive
  // in tmux — re-join them (rows with a missing window → 'gone' + card
  // offline; scoped fd<PORT>-* windows with no row → /state spawn_orphans).
  core.reconcileSpawns().catch(err => console.error('fleetd spawn reconciliation error:', err));
  startAgentsPoll(core); // F1 secondary session source; first run shortly after listen
});

function shutdown() {
  try { if (fs.existsSync(PID_FILE) && fs.readFileSync(PID_FILE, 'utf8').trim() === String(process.pid)) fs.unlinkSync(PID_FILE); } catch { /* best effort */ }
  try { db.close(); } catch { /* already closed */ }
  process.exit(0);
}
process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
