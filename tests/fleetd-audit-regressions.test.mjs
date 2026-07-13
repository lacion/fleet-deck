import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { decodeMessage } from '../scripts/fleetd/mdns.mjs';
import { randomPort, spawnRaw } from './helpers/daemon.mjs';

// Three tests below drive fleetd startup through an ESM --experimental-loader
// (helpers/mdns-dgram-loader.mjs) that mocks node:dgram / ./http.mjs / node:os by
// matching the SOURCE module paths (scripts/fleetd/*.mjs). The single-file bundle
// inlines those modules, so the loader intercepts nothing and the mocked
// console-record / mDNS announcement never appears — the tests would hang. They are
// therefore inherently source-only; the daemon behaviour they assert is verified
// against the bundle separately (a real LAN startup elides the token, refuses a
// second same-HOME daemon, and awaits the goodbye), and fully covered here in source
// mode. Skip them when the suite runs against the bundle (npm run test:bundle).
const BUNDLE_SKIP = process.env.FLEETDECK_TEST_DAEMON_SCRIPT
  ? 'source-only: ESM loader mock cannot intercept the inlined bundle'
  : false;

const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

const WAIT_SCALE = Number(process.env.FLEETDECK_TEST_WAIT_SCALE) || 1;

async function waitUntil(predicate, label, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs * WAIT_SCALE;
  while (Date.now() < deadline) {
    const value = predicate();
    if (value) return value;
    await delay(25);
  }
  throw new Error(`timed out waiting for ${label}`);
}

function freshHome(prefix) {
  return mkdtempSync(path.join(tmpdir(), prefix));
}

function loaderOptions(extra = {}) {
  const loader = path.resolve('tests/helpers/mdns-dgram-loader.mjs');
  return {
    NODE_OPTIONS: `${process.env.NODE_OPTIONS || ''} --no-warnings --experimental-loader=${pathToFileURL(loader).href}`.trim(),
    ...extra,
  };
}

test('LAN startup logs elide the token and direct operators to the share panel', { skip: BUNDLE_SKIP }, async (t) => {
  const token = 'audit-token-must-never-reach-fleetd-log-0123456789';
  const home = freshHome('fleetdeck-token-log-');
  const consoleRecord = path.join(home, 'console.log');
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const daemon = spawnRaw({
    port: randomPort(),
    home,
    env: loaderOptions({
      FLEETDECK_BIND: '0.0.0.0',
      FLEETDECK_TOKEN: token,
      FLEETDECK_MDNS: 'off',
      FLEETDECK_TEST_CONSOLE_RECORD: consoleRecord,
    }),
  });
  t.after(() => daemon.kill());

  const output = await waitUntil(() => {
    if (daemon.proc.exitCode !== null) throw new Error(`daemon exited ${daemon.proc.exitCode}:\n${daemon.stdout}\n${daemon.stderr}`);
    try {
      const text = readFileSync(consoleRecord, 'utf8');
      // The LAN line is a separate console.log after the up-banner; waiting only
      // for 'fleetd up on' races the second write and flakes on slow runners.
      return text.includes('credential available in share panel') ? text : null;
    } catch { return null; }
  }, 'startup banner incl. LAN line');
  assert.equal(output.includes(token), false, `credential leaked in startup logs:\n${output}`);
  assert.match(output, /fleetd LAN http:\/\/[^\s]+\/\?t=<hidden>/);
  assert.match(output, /credential available in share panel/);
});

test('one FLEETDECK_HOME cannot be opened concurrently by daemons on different ports', { skip: BUNDLE_SKIP }, async (t) => {
  const home = freshHome('fleetdeck-port-scope-');
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const firstPort = randomPort();
  let secondPort = randomPort();
  while (secondPort === firstPort) secondPort = randomPort();
  const consoleRecord = path.join(home, 'console.log');

  const first = spawnRaw({ port: firstPort, home, env: loaderOptions({ FLEETDECK_TEST_CONSOLE_RECORD: consoleRecord }) });
  t.after(() => first.kill());
  await waitUntil(() => {
    if (first.proc.exitCode !== null) throw new Error(`daemon exited ${first.proc.exitCode}:\n${first.stdout}\n${first.stderr}`);
    try { return readFileSync(consoleRecord, 'utf8').includes('fleetd up on'); } catch { return false; }
  }, 'first daemon startup');

  const pid = JSON.parse(readFileSync(path.join(home, 'fleetd.pid'), 'utf8'));
  assert.deepEqual(pid, { pid: first.proc.pid, port: firstPort }, 'pidfile records the HOME owner and its port');

  const second = spawnRaw({ port: secondPort, home, env: loaderOptions() });
  t.after(() => second.kill());
  const code = await second.waitForExit(5000);
  assert.equal(code, 1, `second daemon unexpectedly started:\n${second.stdout}\n${second.stderr}`);
  assert.match(second.stderr, new RegExp(`already used by live fleetd pid .* port ${firstPort}`));

  assert.equal(first.proc.exitCode, null, 'refusing the second port must not disturb the HOME owner');
});

test('Linux PID reuse by a non-fleetd process does not retain a stale HOME lock', {
  skip: process.platform !== 'linux' ? 'requires Linux /proc process metadata' : false,
}, async (t) => {
  const home = freshHome('fleetdeck-recycled-pid-');
  const pidFile = path.join(home, 'fleetd.pid');
  t.after(() => rmSync(home, { recursive: true, force: true }));

  // The test runner PID is live and node-backed, but its cmdline is not fleetd.
  // A stale record for it models OS PID reuse without needing privileged PID
  // namespace control. The short token then fails before any socket bind.
  writeFileSync(pidFile, JSON.stringify({ pid: process.pid, port: randomPort() }));
  const daemon = spawnRaw({
    port: randomPort(),
    home,
    env: loaderOptions({ FLEETDECK_TOKEN: 'too-short' }),
  });
  t.after(() => daemon.kill());

  const code = await daemon.waitForExit(5000);
  assert.equal(code, 1);
  assert.match(daemon.stderr, /FLEETDECK_TOKEN must be at least 16 characters/,
    `recycled PID was mistaken for fleetd:\n${daemon.stderr}`);
  assert.equal(existsSync(pidFile), false, 'startupFatal must release the newly claimed pidfile');
});

test('SIGTERM waits for the mDNS goodbye send callback before fleetd exits', { skip: BUNDLE_SKIP }, async (t) => {
  const home = freshHome('fleetdeck-goodbye-');
  const record = path.join(home, 'mdns.jsonl');
  const port = randomPort();
  t.after(() => rmSync(home, { recursive: true, force: true }));

  const child = spawnRaw({
    port,
    home,
    env: loaderOptions({
      FLEETDECK_BIND: '0.0.0.0',
      FLEETDECK_TOKEN: 'goodbye-race-token-0123456789abcdef',
      FLEETDECK_MDNS_RECORD: record,
      FLEETDECK_MDNS_SEND_DELAY_MS: '175',
    }),
  });
  t.after(() => child.kill());

  await waitUntil(() => {
    try { return readFileSync(record, 'utf8').includes('"type":"send"'); } catch { return false; }
  }, 'initial mocked mDNS announcement');

  child.proc.kill('SIGTERM');
  const code = await child.waitForExit(5000);
  assert.equal(code, 0, `fleetd did not shut down cleanly:\n${child.stdout}\n${child.stderr}`);

  const records = readFileSync(record, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse);
  const goodbyeSends = records.filter(item => {
    if (item.type !== 'send') return false;
    const packet = decodeMessage(Buffer.from(item.wire, 'base64'));
    return packet?.answers.length > 0 && packet.answers.every(answer => answer.ttl === 0);
  });
  assert.equal(goodbyeSends.length, 1, 'signal shutdown must enqueue exactly one TTL-0 goodbye');
  const goodbye = decodeMessage(Buffer.from(goodbyeSends[0].wire, 'base64'));
  assert.ok(goodbye.answers.some(answer => answer.typeName === 'PTR'
    && /^Fleet Deck [0-9a-f]{6}\._fleetdeck\._tcp\.local$/.test(answer.data)),
  'DNS-SD instance uses a random discriminator instead of the OS hostname');
  assert.ok(records.some(item => item.type === 'callback' && item.wire === goodbyeSends[0].wire),
    'fleetd must remain alive until the goodbye send callback runs');
});
