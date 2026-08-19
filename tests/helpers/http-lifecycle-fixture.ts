#!/usr/bin/env bun

// Process boundary for the HTTP lifecycle contract. Keeping Bun.serve in a
// child prevents the test runner's own server/WebSocket bookkeeping from
// becoming part of the resource graph under test.

import readline from 'node:readline';
import path from 'node:path';
import { openDb } from '../../src/daemon/db.ts';
import { createCore } from '../../src/daemon/derive.ts';
import { createHttp } from '../../src/daemon/http.ts';

interface ClosedMessage {
  type: 'closed';
  sharedClosePromise: boolean;
  ownedCounts: ReturnType<ReturnType<typeof createHttp>['lifecycle']['ownedCounts']>;
}

interface CountsMessage {
  type: 'counts';
  ownedCounts: ReturnType<ReturnType<typeof createHttp>['lifecycle']['ownedCounts']>;
}

function emit(
  message: { type: 'ready'; port: number; pid: number } | ClosedMessage | CountsMessage,
): void {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function listen(handle: ReturnType<typeof createHttp>, port: number): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    handle.server.once('error', reject);
    handle.server.listen(port, '127.0.0.1', resolve);
  });
}

const [home, portText, token] = process.argv.slice(2);
const port = Number(portText);
if (!home || !token || !Number.isSafeInteger(port) || port < 1 || port > 65_535) {
  throw new Error('usage: http-lifecycle-fixture.ts HOME PORT TOKEN');
}

const db = openDb(path.join(home, 'fleetd.db'));
const core = createCore(db, { port, home, holdMs: 30_000, version: '0.0.0-test' });
const http = createHttp(core, { port, token, version: '0.0.0-test' });
let shutdownPromise: Promise<ClosedMessage> | null = null;

function shutdown(): Promise<ClosedMessage> {
  shutdownPromise ??= (async () => {
    const first = http.lifecycle.close();
    const second = http.lifecycle.close();
    const sharedClosePromise = first === second;
    await first;
    const afterSettlement = http.lifecycle.close();
    await afterSettlement;
    await core.lifecycle.close();
    db.close();
    return {
      type: 'closed',
      sharedClosePromise: sharedClosePromise && afterSettlement === first,
      ownedCounts: http.lifecycle.ownedCounts(),
    };
  })();
  return shutdownPromise;
}

try {
  await listen(http, port);
  emit({ type: 'ready', port, pid: process.pid });

  const input = readline.createInterface({ input: process.stdin });
  for await (const line of input) {
    const command = line.trim();
    if (command === 'counts') {
      emit({ type: 'counts', ownedCounts: http.lifecycle.ownedCounts() });
    } else if (command === 'close') {
      emit(await shutdown());
    }
  }

  // EOF is also a cleanup command, which keeps failed parent assertions from
  // stranding a listener if the fixture is allowed to exit naturally.
  await shutdown();
} catch (error) {
  try {
    await shutdown();
  } catch {
    /* preserve the original fixture failure */
  }
  const detail = error instanceof Error ? (error.stack ?? error.message) : String(error);
  process.stderr.write(`${detail}\n`);
  process.exitCode = 1;
}
