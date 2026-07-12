import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { decodeMessage } from '../scripts/fleetd/mdns.mjs';
import { randomPort, spawnRaw } from './helpers/daemon.mjs';

const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

async function waitUntil(predicate, label, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
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

test('LAN startup logs elide the token and direct operators to the share panel', async (t) => {
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
      return text.includes('fleetd up on') ? text : null;
    } catch { return null; }
  }, 'startup banner');
  assert.equal(output.includes(token), false, `credential leaked in startup logs:\n${output}`);
  assert.match(output, /fleetd LAN http:\/\/[^\s]+\/\?t=<hidden>/);
  assert.match(output, /credential available in share panel/);
});

test('one FLEETDECK_HOME cannot be opened concurrently by daemons on different ports', async (t) => {
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

test('SIGTERM waits for the mDNS goodbye send callback before fleetd exits', async (t) => {
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
