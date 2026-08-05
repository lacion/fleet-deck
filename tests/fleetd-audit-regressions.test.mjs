import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync, appendFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { decodeMessage } from '../scripts/fleetd/mdns.mjs';
import { randomPort, spawnRaw } from './helpers/daemon.mjs';
import { waitUntil as waitUntilBase } from './helpers/wait.mjs';

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

// Positional-signature adapter over the shared scaled poller: call sites pass
// (predicate, label) with an authored 5000ms budget and a 25ms poll.
const waitUntil = (predicate, label, timeoutMs = 5000) =>
  waitUntilBase(predicate, { label, timeoutMs, intervalMs: 25 });

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
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
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
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
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
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

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

test('a disabled mDNS responder keeps the .local URL out of the startup banner', { skip: BUNDLE_SKIP }, async (t) => {
  // BUG-122: fleetd printed the mDNS success line immediately after start(),
  // but bind + multicast membership resolve asynchronously — with no multicast
  // route the responder disables itself one tick later and the banner (and
  // share panel) kept offering a URL that could never resolve. The banner may
  // only appear on a tick where the responder is actually alive.
  const home = freshHome('fleetdeck-mdns-banner-');
  const consoleRecord = path.join(home, 'console.log');
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const daemon = spawnRaw({
    port: randomPort(),
    home,
    env: loaderOptions({
      FLEETDECK_BIND: '0.0.0.0',
      FLEETDECK_TOKEN: 'mdns-banner-token-0123456789abcdef',
      FLEETDECK_MDNS_JOIN_FAILS: '1', // loader seam: every membership join fails
      FLEETDECK_TEST_CONSOLE_RECORD: consoleRecord,
    }),
  });
  t.after(() => daemon.kill());

  // Wait for the disable to be OBSERVABLE (stderr), then read the recorded
  // stdout: the disable happens one tick after listen, exactly when a fix that
  // checks aliveness would gate the banner — so ordering is settled by now.
  await waitUntil(() => {
    if (daemon.proc.exitCode !== null) throw new Error(`daemon exited ${daemon.proc.exitCode}:\n${daemon.stdout}\n${daemon.stderr}`);
    return daemon.stderr.includes('mdns disabled') || null;
  }, 'mocked mDNS responder to disable itself');

  const output = readFileSync(consoleRecord, 'utf8');
  assert.equal(output.includes('.local'), false, `banner advertised an unresolvable .local URL:\n${output}`);
  assert.match(output, /fleetd LAN http:\/\/192\.0\.2\.77:\d+\/\?t=<hidden>/, 'the IP URLs must still be announced');
});

test('a live mDNS responder still gets its .local URL into the startup banner', { skip: BUNDLE_SKIP }, async (t) => {
  // The complement: gating the banner on aliveness must not silence the
  // healthy path — with the default mock (joins succeed) the line survives.
  const home = freshHome('fleetdeck-mdns-banner-ok-');
  const consoleRecord = path.join(home, 'console.log');
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const daemon = spawnRaw({
    port: randomPort(),
    home,
    env: loaderOptions({
      FLEETDECK_BIND: '0.0.0.0',
      FLEETDECK_TOKEN: 'mdns-banner-token-0123456789abcdef',
      FLEETDECK_TEST_CONSOLE_RECORD: consoleRecord,
    }),
  });
  t.after(() => daemon.kill());

  await waitUntil(() => {
    if (daemon.proc.exitCode !== null) throw new Error(`daemon exited ${daemon.proc.exitCode}:\n${daemon.stdout}\n${daemon.stderr}`);
    return daemon.stderr.includes('mdns responding for') || null;
  }, 'mocked mDNS responder to come up');

  const output = readFileSync(consoleRecord, 'utf8');
  assert.match(output, /fleetd LAN http:\/\/fleetdeck\.local:\d+\/\?t=<hidden> \(mDNS; credential available in share panel\)/,
    `healthy responder lost its banner line:\n${output}`);
});

test('SIGTERM waits for the mDNS goodbye send callback before fleetd exits', { skip: BUNDLE_SKIP }, async (t) => {
  const home = freshHome('fleetdeck-goodbye-');
  const record = path.join(home, 'mdns.jsonl');
  const port = randomPort();
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

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

<<<<<<< /tmp/mf-ours

test('a LAN address change refreshes discovery: mDNS retires the old address and announces the new one (BUG-118)', { skip: BUNDLE_SKIP }, async (t) => {
  const home = freshHome('fleetdeck-lan-roam-');
  const record = path.join(home, 'mdns.jsonl');
  const consoleRecord = path.join(home, 'console.log');
  const netFile = path.join(home, 'net.json');
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  const child = spawnRaw({
=======
test('a dotted FLEETDECK_MDNS_NAME yields ONE canonical host: banner, log and advertisement agree', { skip: BUNDLE_SKIP }, async (t) => {
  // BUG-120: the responder canonicalizes a raw name like "team.deck" to the
  // "team-deck.local" it can legally advertise. Anything else the daemon says
  // about the mDNS host — the startup banner and the LAN log line (and, behind
  // them, /state's lan.mdns and the Host allowlist built from it) — must name
  // that SAME host, or the share URL resolves nowhere and the advertised Host
  // is rejected with 403.
  const home = freshHome('fleetdeck-mdns-canonical-');
  const record = path.join(home, 'mdns.jsonl');
  const consoleRecord = path.join(home, 'console.log');
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const daemon = spawnRaw({
>>>>>>> /tmp/mf-theirs
    port: randomPort(),
    home,
    env: loaderOptions({
      FLEETDECK_BIND: '0.0.0.0',
<<<<<<< /tmp/mf-ours
      FLEETDECK_TOKEN: 'lan-roam-token-0123456789abcdef',
      FLEETDECK_MDNS_RECORD: record,
      FLEETDECK_MDNS_SEND_DELAY_MS: '0',
      FLEETDECK_TEST_CONSOLE_RECORD: consoleRecord,
      FLEETDECK_TEST_NET_FILE: netFile,
      FLEETDECK_LAN_REFRESH_MS: '100',
    }),
  });
  t.after(() => child.kill());

  const packets = () => {
    try {
      return readFileSync(record, 'utf8').trim().split('\n').filter(Boolean)
        .map(JSON.parse)
        .filter(item => item.type === 'send')
        .map(item => decodeMessage(Buffer.from(item.wire, 'base64')))
        .filter(Boolean);
    } catch { return []; }
  };

  // Startup announces the mocked network-A address (192.0.2.77).
  await waitUntil(() => {
    if (child.proc.exitCode !== null) throw new Error(`daemon exited ${child.proc.exitCode}:\n${child.stdout}\n${child.stderr}`);
    return packets().some(p => p.isResponse && p.answers.some(r => r.typeName === 'A' && r.data === '192.0.2.77' && r.ttl > 0)) || null;
  }, 'startup announcement for network A');

  // The roam: the mocked os.networkInterfaces() starts answering 198.51.100.88.
  writeFileSync(netFile, JSON.stringify([{ family: 'IPv4', internal: false, address: '198.51.100.88' }]));

  // The watcher must retire the OLD address (TTL-0 A goodbye) and announce the NEW one.
  await waitUntil(() => {
    const sent = packets();
    const goodbye = sent.some(p => p.isResponse && p.answers.length > 0
      && p.answers.every(r => r.ttl === 0)
      && p.answers.some(r => r.typeName === 'A' && r.data === '192.0.2.77'));
    const announced = sent.some(p => p.isResponse
      && p.answers.some(r => r.typeName === 'A' && r.data === '198.51.100.88' && r.ttl > 0));
    return (goodbye && announced) || null;
  }, 'mDNS goodbye for network A and announcement for network B');

  // The roam is observable in the startup log too. (refreshLan lines recorded by
  // the http mock carry the token BY DESIGN — the share panel's credentialed URLs
  // are its whole point; the token-log contract only covers the console lines.)
  const log = readFileSync(consoleRecord, 'utf8');
  const consoleLines = log.split('\n').filter(line => !line.startsWith('refreshLan ')).join('\n');
  assert.match(consoleLines, /fleetd LAN addresses now 198\.51\.100\.88/, `roam was not logged:\n${log}`);
  assert.equal(consoleLines.includes('lan-roam-token-0123456789abcdef'), false, 'the token must never reach a console line');
  assert.match(log, /refreshLan .*198\.51\.100\.88/, `share-panel LAN state did not follow the roam:\n${log}`);
=======
      FLEETDECK_TOKEN: 'mdns-canonical-token-0123456789abcdef',
      FLEETDECK_MDNS_NAME: 'team.deck',
      FLEETDECK_MDNS_RECORD: record,
      FLEETDECK_TEST_CONSOLE_RECORD: consoleRecord,
    }),
  });
  t.after(() => daemon.kill());

  await waitUntil(() => {
    if (daemon.proc.exitCode !== null) throw new Error(`daemon exited ${daemon.proc.exitCode}:\n${daemon.stdout}\n${daemon.stderr}`);
    try { return readFileSync(record, 'utf8').includes('"type":"send"'); } catch { return false; }
  }, 'initial mocked mDNS announcement');

  const send = readFileSync(record, 'utf8').trim().split('\n').filter(Boolean)
    .map(JSON.parse).find(item => item.type === 'send');
  const packet = decodeMessage(Buffer.from(send.wire, 'base64'));
  const aRecord = packet.answers.find(answer => answer.typeName === 'A');
  assert.equal(aRecord.name, 'team-deck.local', 'the wire advertises the canonicalized label');

  const output = await waitUntil(() => {
    try {
      const text = readFileSync(consoleRecord, 'utf8');
      return text.includes('(mDNS;') ? text : null;
    } catch { return null; }
  }, 'mDNS LAN log line');
  assert.match(output, /fleetd LAN http:\/\/team-deck\.local:\d+\/\?t=<hidden> \(mDNS;/,
    `the log must name the advertised host, not the raw env value:\n${output}`);
  assert.ok(!output.includes('team.deck.local'), `the unsplittable raw name must never be printed:\n${output}`);
>>>>>>> /tmp/mf-theirs
});
