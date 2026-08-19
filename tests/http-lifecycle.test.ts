import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
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
