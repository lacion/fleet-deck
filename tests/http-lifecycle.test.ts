import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import net, { type AddressInfo, type Socket } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from '../src/daemon/db.ts';
import { createCore } from '../src/daemon/derive.ts';
import { createHttp } from '../src/daemon/http.ts';
import test from './helpers/harness-test.ts';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..');
const HTTP_LIFECYCLE_FIXTURE = path.join(HERE, 'helpers/http-lifecycle-fixture.ts');
const TERM_LIFECYCLE_FIXTURE = path.join(HERE, 'helpers/termbridge-lifecycle-fixture.ts');
const ZERO_OWNED_COUNTS = {
  listener: 0,
  snapshotClients: 0,
  terminalClients: 0,
  activeResponses: 0,
  watchWaiters: 0,
  terminalOpens: 0,
  broadcastTimers: 0,
  keepaliveTimers: 0,
};

interface FixtureReady {
  type: 'ready';
  port: number;
  pid: number;
}

interface FixtureClosed {
  type: 'closed';
  sharedClosePromise: boolean;
  ownedCounts: typeof ZERO_OWNED_COUNTS;
}

interface FixtureCounts {
  type: 'counts';
  ownedCounts: typeof ZERO_OWNED_COUNTS;
}

type FixtureMessage = FixtureReady | FixtureClosed | FixtureCounts;

async function freePort(): Promise<number> {
  const probe = http.createServer();
  await new Promise<void>((resolve, reject) => {
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', resolve);
  });
  const port = (probe.address() as AddressInfo).port;
  await new Promise<void>((resolve) => probe.close(() => resolve()));
  return port;
}

function openWebSocket(url: string): Promise<{ socket: WebSocket; closed: Promise<void> }> {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    let open = false;
    let snapshot = false;
    let settled = false;
    const closed = new Promise<void>((closedResolve) => {
      socket.addEventListener('close', () => closedResolve(), { once: true });
    });
    const finish = (): void => {
      if (settled || !open || !snapshot) return;
      settled = true;
      resolve({ socket, closed });
    };
    socket.addEventListener(
      'error',
      () => {
        if (!settled) reject(new Error('websocket open failed'));
      },
      { once: true },
    );
    socket.addEventListener(
      'open',
      () => {
        open = true;
        finish();
      },
      { once: true },
    );
    socket.addEventListener('message', (event) => {
      if (typeof event.data !== 'string') return;
      const frame = JSON.parse(event.data) as { type?: unknown };
      if (frame.type !== 'snapshot') return;
      snapshot = true;
      finish();
    });
  });
}

async function openTerminalWebSocket(
  url: string,
): Promise<{ socket: WebSocket; closed: Promise<void> }> {
  const socket = new WebSocket(url);
  const closed = new Promise<void>((resolve) => {
    socket.addEventListener('close', () => resolve(), { once: true });
  });
  await within(
    new Promise<void>((resolve, reject) => {
      let initialized = false;
      socket.addEventListener(
        'error',
        () => {
          if (!initialized) reject(new Error('terminal websocket open failed'));
        },
        { once: true },
      );
      socket.addEventListener(
        'close',
        () => {
          if (!initialized) reject(new Error('terminal websocket closed before init'));
        },
        { once: true },
      );
      socket.addEventListener('message', (event) => {
        if (typeof event.data !== 'string') return;
        const frame = JSON.parse(event.data) as { t?: unknown };
        if (frame.t !== 'init') return;
        initialized = true;
        resolve();
      });
    }),
    'terminal websocket init',
  );
  return { socket, closed };
}

interface TermFixtureRecord {
  readonly pid: number;
  readonly type: string;
  readonly signal?: string;
}

function termFixtureRecords(file: string): TermFixtureRecord[] {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line) => JSON.parse(line) as TermFixtureRecord);
}

function processAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function waitUntil(
  predicate: () => boolean,
  label: string,
  timeoutMs = 1_000,
): Promise<void> {
  const deadline = performance.now() + timeoutMs;
  while (!predicate()) {
    if (performance.now() >= deadline) throw new Error(`timed out waiting for ${label}`);
    await Bun.sleep(5);
  }
}

async function* fixtureMessages(
  stream: ReadableStream<Uint8Array>,
): AsyncGenerator<FixtureMessage> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let buffered = '';
  try {
    while (true) {
      const { done, value } = await reader.read();
      buffered += decoder.decode(value, { stream: !done });
      let newline = buffered.indexOf('\n');
      while (newline >= 0) {
        const line = buffered.slice(0, newline).trim();
        buffered = buffered.slice(newline + 1);
        if (line) yield JSON.parse(line) as FixtureMessage;
        newline = buffered.indexOf('\n');
      }
      if (done) {
        const tail = buffered.trim();
        if (tail) yield JSON.parse(tail) as FixtureMessage;
        return;
      }
    }
  } finally {
    reader.releaseLock();
  }
}

function within<T>(promise: Promise<T>, label: string, timeoutMs = 5_000): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`timed out waiting for ${label}`)), timeoutMs);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error: unknown) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

async function nextFixtureMessage(
  iterator: AsyncIterator<FixtureMessage>,
  label: string,
): Promise<FixtureMessage> {
  const next = await within(iterator.next(), label);
  if (next.done) throw new Error(`fixture exited before ${label}`);
  return next.value;
}

interface HookResponse {
  status: number;
  body: unknown;
}

function hook(base: string, token: string, body: Record<string, unknown>): Promise<HookResponse> {
  const payload = JSON.stringify(body);
  const url = new URL(`/hook/${String(body['hook_event_name'])}`, base);
  return new Promise<HookResponse>((resolve, reject) => {
    const request = http.request(
      url,
      {
        method: 'POST',
        agent: false,
        headers: {
          authorization: `Bearer ${token}`,
          connection: 'close',
          'content-type': 'application/json',
          'content-length': Buffer.byteLength(payload),
        },
      },
      (response) => {
        const chunks: Buffer[] = [];
        response.on('data', (chunk: Buffer) => chunks.push(chunk));
        response.once('error', reject);
        response.once('end', () => {
          const text = Buffer.concat(chunks).toString('utf8');
          resolve({
            status: response.statusCode ?? 0,
            body: text ? JSON.parse(text) : null,
          });
        });
      },
    );
    request.once('error', reject);
    request.end(payload);
  });
}

async function withheldHook(
  port: number,
  token: string,
): Promise<{ socket: Socket; response: Promise<HookResponse> }> {
  const socket = net.createConnection({ host: '127.0.0.1', port });
  await new Promise<void>((resolve, reject) => {
    socket.once('connect', resolve);
    socket.once('error', reject);
  });

  const response = new Promise<HookResponse>((resolve, reject) => {
    const chunks: Buffer[] = [];
    let settled = false;
    const finish = (): void => {
      if (settled) return;
      settled = true;
      const raw = Buffer.concat(chunks).toString('utf8');
      const boundary = raw.indexOf('\r\n\r\n');
      const status = /^HTTP\/1\.1\s+(\d+)/.exec(raw)?.[1];
      if (boundary < 0 || !status) {
        reject(new Error(`invalid withheld-hook response: ${JSON.stringify(raw)}`));
        return;
      }
      const text = raw.slice(boundary + 4);
      resolve({ status: Number(status), body: text ? JSON.parse(text) : null });
    };
    socket.on('data', (chunk: Buffer) => chunks.push(chunk));
    socket.once('end', finish);
    socket.once('close', finish);
    socket.once('error', reject);
  });

  const prefix = JSON.stringify({
    hook_event_name: 'PermissionRequest',
    session_id: 'withheld-http-lifecycle-session',
  });
  socket.write(
    [
      'POST /hook/PermissionRequest HTTP/1.1',
      `Host: 127.0.0.1:${port}`,
      `Authorization: Bearer ${token}`,
      'Content-Type: application/json',
      `Content-Length: ${Buffer.byteLength(prefix) + 4096}`,
      'Connection: close',
      '',
      prefix,
    ].join('\r\n'),
  );
  return { socket, response };
}

async function openHeldHookSocket(
  port: number,
  token: string,
  sessionId: string,
): Promise<{ socket: Socket; closed: Promise<void> }> {
  const socket = net.createConnection({ host: '127.0.0.1', port });
  const closed = new Promise<void>((resolve) => socket.once('close', () => resolve()));
  await new Promise<void>((resolve, reject) => {
    socket.once('connect', resolve);
    socket.once('error', reject);
  });
  // The complete body drains before the peer is aborted, leaving a held route
  // whose response can no longer finish naturally. This is distinct from the
  // withheld-body case: shutdown must notice the response-side abort even after
  // the request-side pump has settled.
  const payload = JSON.stringify({
    hook_event_name: 'PermissionRequest',
    session_id: sessionId,
    tool_name: 'Bash',
    tool_input: { command: 'printf abandoned' },
  });
  socket.write(
    [
      'POST /hook/PermissionRequest HTTP/1.1',
      `Host: 127.0.0.1:${port}`,
      `Authorization: Bearer ${token}`,
      'Content-Type: application/json',
      `Content-Length: ${Buffer.byteLength(payload)}`,
      'Connection: keep-alive',
      '',
      payload,
    ].join('\r\n'),
  );
  return { socket, closed };
}

async function closeCore(core: ReturnType<typeof createCore>): Promise<void> {
  const lifecycle = (core as typeof core & { lifecycle?: { close: () => void | Promise<void> } })
    .lifecycle;
  await lifecycle?.close();
}

test('HTTP lifecycle releases a held hook as 200 {}, closes WS, and awaits listener release', async (t) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-http-lifecycle-'));
  const port = await freePort();
  const token = '0123456789abcdef0123456789abcdef';
  const base = `http://127.0.0.1:${port}`;
  const child = Bun.spawn([process.execPath, HTTP_LIFECYCLE_FIXTURE, home, String(port), token], {
    cwd: ROOT,
    env: {
      ...process.env,
      FLEETDECK_HOLD_SCOPE: 'all',
      FLEETDECK_TERM: 'off',
    },
    stdin: 'pipe',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stderr = Bun.readableStreamToText(child.stderr);
  const messages = fixtureMessages(child.stdout)[Symbol.asyncIterator]();
  let withheldSocket: Socket | null = null;
  t.after(async () => {
    withheldSocket?.destroy();
    try {
      await child.stdin.end();
    } catch {
      /* already closed */
    }
    try {
      await within(child.exited, 'HTTP lifecycle fixture exit', 2_000);
    } catch {
      child.kill('SIGKILL');
      await child.exited;
    }
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const ready = await nextFixtureMessage(messages, 'fixture readiness');
  assert.equal(ready.type, 'ready');
  assert.equal(ready.port, port);
  assert.equal(ready.pid, child.pid);
  const { socket, closed } = await within(
    openWebSocket(`ws://127.0.0.1:${port}/ws?t=${token}`),
    'initial snapshot websocket frame',
  );

  const sessionId = 'http-lifecycle-session';
  const started = await hook(base, token, {
    hook_event_name: 'SessionStart',
    session_id: sessionId,
    cwd: home,
  });
  assert.equal(started.status, 200);

  let settled = false;
  const held = hook(base, token, {
    hook_event_name: 'PermissionRequest',
    session_id: sessionId,
    cwd: home,
    tool_name: 'Bash',
    tool_input: { command: 'printf held' },
  }).then((response) => {
    settled = true;
    return response;
  });
  await Bun.sleep(100);
  assert.equal(settled, false, 'the hook must actually be parked before shutdown');

  const withheld = await withheldHook(port, token);
  withheldSocket = withheld.socket;
  const withheldResponse = within(withheld.response, 'withheld hook shutdown response');
  await Bun.sleep(100);

  child.stdin.write('close\n');
  await child.stdin.flush();

  const response = await within(held, 'held hook release');
  assert.equal(response.status, 200);
  assert.deepEqual(response.body, {});
  const stalledResponse = await withheldResponse;
  assert.equal(stalledResponse.status, 200);
  assert.deepEqual(stalledResponse.body, {});
  await within(closed, 'snapshot websocket close');
  const fixtureClosed = await nextFixtureMessage(messages, 'fixture lifecycle close');
  assert.equal(fixtureClosed.type, 'closed');
  assert.equal(fixtureClosed.sharedClosePromise, true);
  assert.deepEqual(fixtureClosed.ownedCounts, ZERO_OWNED_COUNTS);
  assert.notEqual(socket.readyState, WebSocket.OPEN);

  // Rebind while the fixture process is still alive, proving close() released
  // the listener rather than relying on process exit to return the port.
  assert.equal(child.exitCode, null);
  const rebound = Bun.serve({
    port,
    hostname: '127.0.0.1',
    fetch: () => new Response('ok'),
  });
  await rebound.stop(true);

  await child.stdin.end();
  const exitCode = await within(child.exited, 'HTTP lifecycle fixture natural exit');
  const errorOutput = await stderr;
  assert.equal(exitCode, 0, errorOutput);
});

test('HTTP lifecycle is safe to close before listen and permanently rejects admission', async () => {
  const db = openDb(':memory:');
  const core = createCore(db, { port: 0, home: '/daemon-home' });
  const handle = createHttp(core, { port: 1, token: '0123456789abcdef' });
  try {
    const close = handle.lifecycle.close();
    assert.equal(close, handle.lifecycle.close());
    await close;
    assert.equal(handle.lifecycle.isQuiescing(), true);
    assert.deepEqual(handle.lifecycle.ownedCounts(), ZERO_OWNED_COUNTS);
  } finally {
    await closeCore(core);
    db.close();
  }
});

test('HTTP lifecycle force-settles a drained held response after its client aborts', async (t) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-http-abort-lifecycle-'));
  const port = await freePort();
  const token = 'fedcba9876543210fedcba9876543210';
  const base = `http://127.0.0.1:${port}`;
  const child = Bun.spawn([process.execPath, HTTP_LIFECYCLE_FIXTURE, home, String(port), token], {
    cwd: ROOT,
    env: {
      ...process.env,
      FLEETDECK_HOLD_SCOPE: 'all',
      FLEETDECK_TERM: 'off',
    },
    stdin: 'pipe',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stderr = Bun.readableStreamToText(child.stderr);
  const messages = fixtureMessages(child.stdout)[Symbol.asyncIterator]();
  let heldSocket: Socket | null = null;
  let snapshotSocket: WebSocket | null = null;
  t.after(async () => {
    heldSocket?.destroy();
    snapshotSocket?.close();
    try {
      await child.stdin.end();
    } catch {
      /* already closed */
    }
    try {
      await within(child.exited, 'aborted HTTP fixture exit', 2_000);
    } catch {
      child.kill('SIGKILL');
      await child.exited;
    }
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const ready = await nextFixtureMessage(messages, 'aborted fixture readiness');
  assert.equal(ready.type, 'ready');
  const snapshot = await within(
    openWebSocket(`ws://127.0.0.1:${port}/ws?t=${token}`),
    'aborted fixture snapshot websocket frame',
  );
  snapshotSocket = snapshot.socket;

  const sessionId = 'http-aborted-held-session';
  const started = await hook(base, token, {
    hook_event_name: 'SessionStart',
    session_id: sessionId,
    cwd: home,
  });
  assert.equal(started.status, 200);

  const held = await openHeldHookSocket(port, token, sessionId);
  heldSocket = held.socket;
  await Bun.sleep(100);
  child.stdin.write('counts\n');
  await child.stdin.flush();
  const admitted = await nextFixtureMessage(messages, 'aborted held response admission');
  assert.equal(admitted.type, 'counts');
  assert.equal(admitted.ownedCounts.activeResponses, 1);

  held.socket.destroy();
  await within(held.closed, 'held client abort');
  await Bun.sleep(50);
  child.stdin.write('counts\n');
  await child.stdin.flush();
  const abandoned = await nextFixtureMessage(messages, 'aborted held response tracking');
  assert.equal(abandoned.type, 'counts');
  assert.equal(
    abandoned.ownedCounts.activeResponses,
    1,
    'the disconnected held response remains lifecycle-owned until shutdown settles it',
  );

  child.stdin.write('close\n');
  await child.stdin.flush();
  const fixtureClosed = await nextFixtureMessage(messages, 'aborted fixture lifecycle close');
  assert.equal(fixtureClosed.type, 'closed');
  assert.equal(fixtureClosed.sharedClosePromise, true);
  assert.deepEqual(fixtureClosed.ownedCounts, ZERO_OWNED_COUNTS);
  await within(snapshot.closed, 'aborted fixture snapshot websocket close');

  // The fixture is still alive: successful rebind proves lifecycle.close(), not
  // process teardown, released Bun's listener after the abandoned response.
  assert.equal(child.exitCode, null);
  const rebound = Bun.serve({
    port,
    hostname: '127.0.0.1',
    fetch: () => new Response('ok'),
  });
  await rebound.stop(true);

  await child.stdin.end();
  const exitCode = await within(child.exited, 'aborted HTTP fixture natural exit');
  const errorOutput = await stderr;
  assert.equal(exitCode, 0, errorOutput);
});

test('HTTP typed bind reports success and preserves one successful acquisition identity', async () => {
  const port = await freePort();
  const db = openDb(':memory:');
  const core = createCore(db, { port, home: '/daemon-home' });
  const handle = createHttp(core, { port, token: '0123456789abcdef' });
  try {
    const first = handle.bind(port, '127.0.0.1');
    assert.equal(first, handle.bind(port, '127.0.0.1'));
    const result = await first;
    assert.deepEqual(result, {
      _tag: 'Bound',
      hostname: '127.0.0.1',
      port,
    });

    const health = await fetch(`http://127.0.0.1:${port}/health`);
    assert.equal(health.status, 200);
  } finally {
    await handle.lifecycle.close();
    await closeCore(core);
    db.close();
  }
});

test('HTTP graceful stop shares one completion and releases an idle listener without force', async () => {
  const port = await freePort();
  const db = openDb(':memory:');
  const core = createCore(db, { port, home: '/daemon-home' });
  const handle = createHttp(core, { port, token: '0123456789abcdef' });
  try {
    const bound = await handle.bind(port, '127.0.0.1');
    assert.equal(bound._tag, 'Bound');

    const graceful = handle.lifecycle.beginGracefulStop();
    assert.equal(graceful, handle.lifecycle.beginGracefulStop());
    handle.lifecycle.releaseHolds();
    await within(graceful, 'idle graceful HTTP stop', 1_000);
    assert.equal(handle.lifecycle.ownedCounts().listener, 0);

    // Rebind while the owner process remains alive: stop(false), not process
    // teardown or the forced fallback, returned the listening socket.
    const rebound = Bun.serve({
      port,
      hostname: '127.0.0.1',
      fetch: () => new Response('ok'),
    });
    await rebound.stop(true);
  } finally {
    await handle.lifecycle.close();
    await closeCore(core);
    db.close();
  }
});

test('HTTP graceful stop joins force when force wins before the hold-flush barrier', async () => {
  const port = await freePort();
  const token = '0123456789abcdef';
  const db = openDb(':memory:');
  const core = createCore(db, { port, home: '/daemon-home' });
  const handle = createHttp(core, { port, token });
  let socket: WebSocket | null = null;
  try {
    const bound = await handle.bind(port, '127.0.0.1');
    assert.equal(bound._tag, 'Bound');
    const opened = await within(
      openWebSocket(`ws://127.0.0.1:${port}/ws?t=${token}`),
      'force-before-barrier websocket',
    );
    socket = opened.socket;

    // beginGracefulStop is parked until releaseHolds publishes its flush
    // barrier. Start force first, then open the barrier in the same turn. The
    // graceful operation must join stop(true), never terminate the WebSocket or
    // report listener ownership gone ahead of that native Promise.
    const order: string[] = [];
    const graceful = handle.lifecycle.beginGracefulStop().then(() => {
      order.push('graceful');
    });
    const forced = handle.lifecycle.forceStop().then(() => {
      order.push('force');
    });
    handle.lifecycle.releaseHolds();

    await within(Promise.all([forced, graceful]), 'force-before-barrier HTTP stop', 1_000);
    assert.deepEqual(order, ['force', 'graceful']);
    await within(opened.closed, 'force-before-barrier websocket close', 1_000);
    assert.equal(handle.lifecycle.ownedCounts().listener, 0);
  } finally {
    socket?.close();
    await handle.lifecycle.close();
    await closeCore(core);
    db.close();
  }
});

test('HTTP typed bind preserves Bun EADDRINUSE data and legacy callback timing', async () => {
  const port = await freePort();
  const blocker = Bun.serve({
    port,
    hostname: '127.0.0.1',
    fetch: () => new Response('occupied'),
  });
  const db = openDb(':memory:');
  const core = createCore(db, { port, home: '/daemon-home' });
  const handle = createHttp(core, { port, token: '0123456789abcdef' });
  try {
    const result = await handle.bind(port, '127.0.0.1');
    assert.equal(result._tag, 'BindFailed');
    if (result._tag !== 'BindFailed') throw new Error('expected typed bind failure');
    assert.equal(result.reason, 'address-in-use');
    assert.equal(result.origin, 'bun-serve-throw');
    assert.equal(result.legacyDelivery, 'error-callback-microtask');
    assert.equal(result.code, 'EADDRINUSE');
    assert.equal(result.message, (result.error as Error).message);
    assert.equal(result.errno, (result.error as { errno?: unknown }).errno ?? null);

    let synchronous = true;
    const callbackError = new Promise<NodeJS.ErrnoException>((resolve) => {
      handle.server.once('error', (error) => {
        assert.equal(synchronous, false, 'legacy bind errors stay on a microtask');
        resolve(error);
      });
    });
    handle.server.listen(port, '127.0.0.1');
    synchronous = false;
    const legacy = await within(callbackError, 'legacy EADDRINUSE callback');
    assert.equal(legacy.code, result.code);
    assert.equal(legacy.errno ?? null, result.errno);
    assert.equal(legacy.message, result.message);
  } finally {
    await handle.lifecycle.close();
    await closeCore(core);
    db.close();
    await blocker.stop(true);
  }
});

test('HTTP typed bind returns a structured lifecycle-closed error', async () => {
  const port = await freePort();
  const db = openDb(':memory:');
  const core = createCore(db, { port, home: '/daemon-home' });
  const handle = createHttp(core, { port, token: '0123456789abcdef' });
  try {
    await handle.lifecycle.close();
    const result = await handle.bind(port, '127.0.0.1');
    assert.equal(result._tag, 'BindFailed');
    if (result._tag !== 'BindFailed') throw new Error('expected lifecycle bind failure');
    assert.equal(result.reason, 'closed');
    assert.equal(result.origin, 'lifecycle-guard');
    assert.equal(result.code, 'ERR_SERVER_CLOSED');
    assert.equal(result.errno, null);
    assert.match(result.message, /HTTP lifecycle is closed/);
  } finally {
    await handle.lifecycle.close();
    await closeCore(core);
    db.close();
  }
});

test('HTTP typed bind never replays a cached Bound result after listener close', async () => {
  const port = await freePort();
  const db = openDb(':memory:');
  const core = createCore(db, { port, home: '/daemon-home' });
  const handle = createHttp(core, { port, token: '0123456789abcdef' });
  try {
    const acquired = await handle.bind(port, '127.0.0.1');
    assert.equal(acquired._tag, 'Bound');
    await handle.lifecycle.close();
    assert.equal(handle.lifecycle.ownedCounts().listener, 0);

    const afterClose = await handle.bind(port, '127.0.0.1');
    assert.equal(afterClose._tag, 'BindFailed');
    if (afterClose._tag !== 'BindFailed') throw new Error('expected closed bind failure');
    assert.equal(afterClose.reason, 'closed');
    assert.equal(afterClose.origin, 'lifecycle-guard');
    assert.equal(afterClose.code, 'ERR_SERVER_CLOSED');
  } finally {
    await handle.lifecycle.close();
    await closeCore(core);
    db.close();
  }
});

test('HTTP graceful stop is shared, waits for held-hook flush, and force never serially awaits it', async () => {
  const port = await freePort();
  const token = 'fedcba9876543210fedcba9876543210';
  const base = `http://127.0.0.1:${port}`;
  const db = openDb(':memory:');
  const previousHoldScope = process.env['FLEETDECK_HOLD_SCOPE'];
  let core: ReturnType<typeof createCore>;
  try {
    process.env['FLEETDECK_HOLD_SCOPE'] = 'all';
    core = createCore(db, { port, home: '/daemon-home', holdMs: 30_000 });
  } finally {
    if (previousHoldScope === undefined) delete process.env['FLEETDECK_HOLD_SCOPE'];
    else process.env['FLEETDECK_HOLD_SCOPE'] = previousHoldScope;
  }
  const handle = createHttp(core, { port, token });
  let socket: WebSocket | null = null;
  try {
    const bound = await handle.bind(port, '127.0.0.1');
    assert.equal(bound._tag, 'Bound');
    const opened = await within(
      openWebSocket(`ws://127.0.0.1:${port}/ws?t=${token}`),
      'graceful-stop websocket',
    );
    socket = opened.socket;

    await hook(base, token, {
      hook_event_name: 'SessionStart',
      session_id: 'http-graceful-stop-session',
      cwd: '/daemon-home',
    });
    let hookSettled = false;
    const held = hook(base, token, {
      hook_event_name: 'PermissionRequest',
      session_id: 'http-graceful-stop-session',
      cwd: '/daemon-home',
      tool_name: 'Bash',
      tool_input: { command: 'printf held' },
    }).then((response) => {
      hookSettled = true;
      return response;
    });
    await Bun.sleep(50);
    assert.equal(hookSettled, false);

    const graceful = handle.lifecycle.beginGracefulStop();
    assert.equal(graceful, handle.lifecycle.beginGracefulStop());
    let gracefulSettled = false;
    void graceful.then(
      () => {
        gracefulSettled = true;
      },
      () => {
        gracefulSettled = true;
      },
    );
    await Bun.sleep(25);
    assert.equal(hookSettled, false, 'quiesce does not skip the release-holds phase');
    assert.equal(gracefulSettled, false, 'native stop waits for the held-hook flush barrier');

    handle.lifecycle.releaseHolds();
    const response = await within(held, 'graceful-stop held hook release');
    assert.equal(response.status, 200);
    assert.deepEqual(response.body, {});
    await Bun.sleep(25);
    assert.equal(
      gracefulSettled,
      false,
      'an open WebSocket keeps stop(false) pending after held hooks are writable',
    );

    const clients = handle.lifecycle.closeClients();
    assert.equal(clients, handle.lifecycle.closeClients());
    await within(clients, 'graceful-stop client retirement');

    const forced = handle.lifecycle.forceStop();
    assert.equal(forced, handle.lifecycle.forceStop());
    await within(forced, 'forced HTTP stop', 1_000);
    await within(opened.closed, 'forced websocket close', 1_000);

    const close = handle.lifecycle.close();
    assert.equal(handle.lifecycle.forceStop(), forced, 'close reuses the force-stop completion');
    await within(close, 'full HTTP close after force', 1_000);
    assert.deepEqual(handle.lifecycle.ownedCounts(), ZERO_OWNED_COUNTS);
  } finally {
    socket?.close();
    await handle.lifecycle.close();
    await closeCore(core);
    db.close();
  }
});

test('HTTP client force reaps a TERM-resistant terminal while a response join is stuck', async (t) => {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-http-client-force-'));
  const record = path.join(scratch, 'terminal.jsonl');
  const port = await freePort();
  const token = '0123456789abcdef0123456789abcdef';
  const base = `http://127.0.0.1:${port}`;
  const sessionId = 'http-client-force-session';
  const spawnId = 'http-client-force-spawn';
  const controlledEnvironment = {
    FLEETDECK_HOLD_SCOPE: 'all',
    FLEETDECK_TERM_CMD: TERM_LIFECYCLE_FIXTURE,
    FLEETDECK_TEST_TERM_LIFECYCLE_RECORD: record,
    FLEETDECK_TEST_TERM_LIFECYCLE_IGNORE_SIGTERM: '1',
  };
  const previousEnvironment = new Map<string, string | undefined>();
  for (const [key, value] of Object.entries(controlledEnvironment)) {
    previousEnvironment.set(key, process.env[key]);
    process.env[key] = value;
  }

  const db = openDb(':memory:');
  const core = createCore(db, { port, home: scratch, holdMs: 30_000 });
  const handle = createHttp(core, { port, token });
  let terminalSocket: WebSocket | null = null;
  let snapshotSocket: WebSocket | null = null;
  let controlPid: number | null = null;
  t.after(async () => {
    terminalSocket?.close();
    snapshotSocket?.close();
    try {
      handle.lifecycle.releaseHolds();
      handle.lifecycle.forceClients();
      await handle.lifecycle.forceStop();
      await handle.lifecycle.close();
    } catch {
      /* preserve the test failure */
    }
    await closeCore(core);
    db.close();
    if (controlPid && processAlive(controlPid)) {
      try {
        process.kill(controlPid, 'SIGKILL');
      } catch {
        /* already gone */
      }
    }
    for (const [key, value] of previousEnvironment) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
    rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const bound = await handle.bind(port, '127.0.0.1');
  assert.equal(bound._tag, 'Bound');
  const snapshot = await openWebSocket(`ws://127.0.0.1:${port}/ws?t=${token}`);
  snapshotSocket = snapshot.socket;
  const started = await hook(base, token, {
    hook_event_name: 'SessionStart',
    session_id: sessionId,
    cwd: scratch,
  });
  assert.equal(started.status, 200);
  db.prepare(
    `INSERT INTO spawns
      (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, requested_at, status, skip_permissions, remote_control, gateway, kind)
      VALUES (?, ?, ?, ?, ?, ?, ?, 'live', 0, 0, 0, 'claude')`,
  ).run(
    spawnId,
    sessionId,
    'client-force',
    `fleetdeck-${port}`,
    `fd${port}-client-force`,
    scratch,
    Date.now(),
  );

  const terminal = await openTerminalWebSocket(
    `ws://127.0.0.1:${port}/ws/term?spawn=${spawnId}&cols=80&rows=24&t=${token}`,
  );
  terminalSocket = terminal.socket;
  await waitUntil(() => {
    controlPid = termFixtureRecords(record).find((entry) => entry.type === 'start')?.pid ?? null;
    return controlPid !== null;
  }, 'terminal control child');
  if (controlPid === null) throw new Error('terminal control fixture did not publish its pid');
  const ownedControlPid = controlPid;

  let heldSettled = false;
  const held = hook(base, token, {
    hook_event_name: 'PermissionRequest',
    session_id: sessionId,
    cwd: scratch,
    tool_name: 'Bash',
    tool_input: { command: 'printf held' },
  }).then((response) => {
    heldSettled = true;
    return response;
  });
  await waitUntil(
    () => handle.lifecycle.ownedCounts().activeResponses === 1,
    'held response ownership',
  );
  assert.equal(heldSettled, false);

  const shutdownStartedAt = performance.now();
  let clientsSettled = false;
  const closingClients = handle.lifecycle.closeClients().then(() => {
    clientsSettled = true;
  });
  await waitUntil(
    () =>
      termFixtureRecords(record).some(
        (entry) => entry.type === 'signal' && entry.signal === 'SIGTERM',
      ),
    'ordinary terminal TERM',
  );

  const callbackStartedAt = performance.now();
  handle.lifecycle.forceClients();
  const callbackElapsedMs = performance.now() - callbackStartedAt;
  assert.ok(callbackElapsedMs < 25, `HTTP forceClients blocked for ${callbackElapsedMs}ms`);
  await waitUntil(() => !processAlive(ownedControlPid), 'forced terminal child reap', 500);
  assert.equal(
    clientsSettled,
    false,
    'the still-held active response proves terminal cleanup did not serialize behind its join',
  );
  assert.equal(handle.lifecycle.ownedCounts().activeResponses, 1);

  handle.lifecycle.releaseHolds();
  const heldResponse = await within(held, 'forced-client held response release');
  assert.equal(heldResponse.status, 200);
  assert.deepEqual(heldResponse.body, {});
  await within(closingClients, 'forced client joins', 500);
  assert.equal(handle.lifecycle.ownedCounts().terminalOpens, 0);

  // Native WebSockets remain owned until stop(true). forceClients must not use
  // close()/terminate() on Bun 1.3.14, whose stop(true) Promise can otherwise
  // remain pending forever.
  assert.notEqual(terminal.socket.readyState, WebSocket.CLOSED);
  await within(handle.lifecycle.forceStop(), 'forced native HTTP stop', 500);
  await within(terminal.closed, 'forced native terminal websocket close', 500);
  await within(handle.lifecycle.close(), 'forced full HTTP close', 500);
  assert.deepEqual(handle.lifecycle.ownedCounts(), ZERO_OWNED_COUNTS);
  assert.equal(processAlive(ownedControlPid), false);
  assert.ok(
    performance.now() - shutdownStartedAt < 1_750,
    'terminal/client/native cleanup must fit the root absolute shutdown deadline',
  );
});
