#!/usr/bin/env bun
// Deterministic cold-start election fixture for tests/takeover.test.ts.
//
// The first process launched for a scratch FLEETDECK_HOME reports the staged
// OLD version; every later process reports the hook's CURRENT version. That is
// the exact externally visible state of a mixed-plugin cold race in which both
// hooks observed an empty port but the older candidate bound first. Keeping the
// scheduling inside this fixture makes the regression deterministic instead of
// relying on two OS processes reaching bind(2) in a particular order.

import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';

const port = Number(process.env['FLEETDECK_PORT']);
const home = process.env['FLEETDECK_HOME'] ?? '';
const oldVersion = process.env['FLEETDECK_COLD_RACE_OLD_VERSION'] ?? '0.0.1';
const currentVersion = process.env['FLEETDECK_COLD_RACE_CURRENT_VERSION'] ?? '99.0.0';
const firstMarker = path.join(home, '.cold-race-first');
const historyFile = path.join(home, '.cold-race-history');
const pidFile = path.join(home, 'fleetd.pid');

fs.mkdirSync(home, { recursive: true });
let version = currentVersion;
try {
  const marker = fs.openSync(firstMarker, 'wx', 0o600);
  fs.closeSync(marker);
  version = oldVersion;
} catch (error) {
  const code =
    typeof error === 'object' && error !== null && typeof Reflect.get(error, 'code') === 'string'
      ? String(Reflect.get(error, 'code'))
      : '';
  if (code !== 'EEXIST') throw error;
}

let listening = false;
let shuttingDown = false;

function removeOwnPidFile(): void {
  try {
    const record = JSON.parse(fs.readFileSync(pidFile, 'utf8')) as { pid?: unknown };
    if (record.pid === process.pid) fs.unlinkSync(pidFile);
  } catch {
    /* absent, replaced, or malformed: never remove another process's claim */
  }
}

const server = http.createServer((request, response) => {
  if (request.method === 'GET' && request.url === '/health') {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ ok: true, pid: process.pid, version, managed: false, fleet: 0 }));
    return;
  }
  if (request.method === 'POST' && request.url === '/hook/SessionStart') {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end('{}');
    return;
  }
  response.writeHead(404);
  response.end();
});

function shutdown(): void {
  if (shuttingDown) return;
  shuttingDown = true;
  if (!listening) {
    removeOwnPidFile();
    process.exit(0);
  }
  server.close(() => {
    removeOwnPidFile();
    process.exit(0);
  });
  setTimeout(() => {
    removeOwnPidFile();
    process.exit(0);
  }, 1000).unref();
}

process.on('SIGTERM', shutdown);
process.on('SIGINT', shutdown);
server.once('error', (error: NodeJS.ErrnoException) => {
  removeOwnPidFile();
  process.exit(error.code === 'EADDRINUSE' ? 3 : 1);
});
server.listen(port, '127.0.0.1', () => {
  listening = true;
  fs.writeFileSync(pidFile, JSON.stringify({ pid: process.pid, port }), {
    encoding: 'utf8',
    mode: 0o600,
  });
  fs.appendFileSync(historyFile, `${version} ${process.pid}\n`, { mode: 0o600 });
});
