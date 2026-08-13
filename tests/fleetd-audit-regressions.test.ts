import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { decodeMessage } from '../scripts/fleetd/mdns.ts';
import { randomPort, spawnRaw } from './helpers/daemon.ts';
import { waitUntil as waitUntilBase } from './helpers/wait.ts';

// decodeMessage's packet/record shapes are file-local to mdns.ts (not exported),
// so the converted assertions derive them from the function's return type rather
// than forcing a source export.
type DecodedMdnsMessage = NonNullable<ReturnType<typeof decodeMessage>>;

// The JSONL the daemon's mocked dgram socket appends (scripts/fleetd/test-seam.ts),
// one object per line. Every field but `type` is emitted only for a subset of
// record kinds ('send'/'callback' carry `wire`; 'setTTL'/'setMulticastTTL' carry
// `value`; 'setiface' carries `address`), so all but `type` are optional.
interface MdnsLogItem {
  type: string;
  value?: number;
  wire?: string;
  address?: string;
}

// The mDNS/banner tests below drive a spawned fleetd whose network stack is mocked
// in-source (test-seam.ts + os-net.ts, armed by FLEETDECK_TEST_NET_MOCK) — the old
// node --experimental-loader is gone (Bun ignores node ESM loader hooks). The seam
// now compiles into the single-file bundle too, so these could in principle run
// against it; they are kept source-only here to hold the change to the runtime swap.
// The bundle path verifies the same daemon behaviour separately (a real LAN startup
// elides the token, refuses a second same-HOME daemon, and awaits the goodbye), and
// it is fully covered here in source mode. Skip when running against the bundle
// (npm run test:bundle).
const BUNDLE_SKIP = process.env['FLEETDECK_TEST_DAEMON_SCRIPT']
  ? 'source-only: held to source mode while the runtime swap settles'
  : false;

// Positional-signature adapter over the shared scaled poller: call sites pass
// (predicate, label) with an authored 5000ms budget and a 25ms poll.
const waitUntil = <T>(
  predicate: () => T | Promise<T>,
  label: string,
  timeoutMs = 5000,
): Promise<NonNullable<Awaited<T>>> =>
  waitUntilBase(predicate, { label, timeoutMs, intervalMs: 25 });

function freshHome(prefix: string): string {
  return mkdtempSync(path.join(tmpdir(), prefix));
}

// Arm the daemon's own in-source network mock (test-seam.ts + os-net.ts) and layer
// on the per-test knobs each case needs (record sinks, TTL/join failure, the
// interface set). FLEETDECK_TEST_NET_MOCK is the master flag; it runs on both Node
// and Bun, unlike the retired `node --experimental-loader` mechanism.
function netMockEnv(extra: Record<string, string> = {}): Record<string, string> {
  return { FLEETDECK_TEST_NET_MOCK: '1', ...extra };
}

test(
  'LAN startup logs elide the token and direct operators to the share panel',
  { skip: BUNDLE_SKIP },
  async (t: TestContext) => {
    const token = 'audit-token-must-never-reach-fleetd-log-0123456789';
    const home = freshHome('fleetdeck-token-log-');
    const consoleRecord = path.join(home, 'console.log');
    t.after(() => {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });
    const daemon = spawnRaw({
      port: randomPort(),
      home,
      env: netMockEnv({
        FLEETDECK_BIND: '0.0.0.0',
        FLEETDECK_TOKEN: token,
        FLEETDECK_MDNS: 'off',
        FLEETDECK_TEST_CONSOLE_RECORD: consoleRecord,
      }),
    });
    t.after(() => daemon.kill());

    const output = await waitUntil(() => {
      if (daemon.proc.exitCode !== null)
        throw new Error(
          `daemon exited ${daemon.proc.exitCode}:\n${daemon.stdout}\n${daemon.stderr}`,
        );
      try {
        const text = readFileSync(consoleRecord, 'utf8');
        // The LAN line is a separate console.log after the up-banner; waiting only
        // for 'fleetd up on' races the second write and flakes on slow runners.
        return text.includes('credential available in share panel') ? text : null;
      } catch {
        return null;
      }
    }, 'startup banner incl. LAN line');
    assert.equal(output.includes(token), false, `credential leaked in startup logs:\n${output}`);
    assert.match(output, /fleetd LAN http:\/\/[^\s]+\/\?t=<hidden>/);
    assert.match(output, /credential available in share panel/);
  },
);

test(
  'one FLEETDECK_HOME cannot be opened concurrently by daemons on different ports',
  { skip: BUNDLE_SKIP },
  async (t: TestContext) => {
    const home = freshHome('fleetdeck-port-scope-');
    t.after(() => {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });
    const firstPort = randomPort();
    let secondPort = randomPort();
    while (secondPort === firstPort) secondPort = randomPort();
    const consoleRecord = path.join(home, 'console.log');

    const first = spawnRaw({
      port: firstPort,
      home,
      env: netMockEnv({ FLEETDECK_TEST_CONSOLE_RECORD: consoleRecord }),
    });
    t.after(() => first.kill());
    await waitUntil(() => {
      if (first.proc.exitCode !== null)
        throw new Error(`daemon exited ${first.proc.exitCode}:\n${first.stdout}\n${first.stderr}`);
      try {
        return readFileSync(consoleRecord, 'utf8').includes('fleetd up on');
      } catch {
        return false;
      }
    }, 'first daemon startup');

    const pid = JSON.parse(readFileSync(path.join(home, 'fleetd.pid'), 'utf8')) as {
      pid: number;
      port: number;
    };
    assert.deepEqual(
      pid,
      { pid: first.proc.pid, port: firstPort },
      'pidfile records the HOME owner and its port',
    );

    const second = spawnRaw({ port: secondPort, home, env: netMockEnv() });
    t.after(() => second.kill());
    const code = await second.waitForExit(5000);
    assert.equal(
      code,
      1,
      `second daemon unexpectedly started:\n${second.stdout}\n${second.stderr}`,
    );
    assert.match(second.stderr, new RegExp(`already used by live fleetd pid .* port ${firstPort}`));

    assert.equal(
      first.proc.exitCode,
      null,
      'refusing the second port must not disturb the HOME owner',
    );
  },
);

test(
  'Linux PID reuse by a non-fleetd process does not retain a stale HOME lock',
  {
    skip: process.platform !== 'linux' ? 'requires Linux /proc process metadata' : false,
  },
  async (t: TestContext) => {
    const home = freshHome('fleetdeck-recycled-pid-');
    const pidFile = path.join(home, 'fleetd.pid');
    t.after(() => {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });

    // The test runner PID is live and node-backed, but its cmdline is not fleetd.
    // A stale record for it models OS PID reuse without needing privileged PID
    // namespace control. The short token then fails before any socket bind.
    writeFileSync(pidFile, JSON.stringify({ pid: process.pid, port: randomPort() }));
    const daemon = spawnRaw({
      port: randomPort(),
      home,
      env: netMockEnv({ FLEETDECK_TOKEN: 'too-short' }),
    });
    t.after(() => daemon.kill());

    const code = await daemon.waitForExit(5000);
    assert.equal(code, 1);
    assert.match(
      daemon.stderr,
      /FLEETDECK_TOKEN must be at least 16 characters/,
      `recycled PID was mistaken for fleetd:\n${daemon.stderr}`,
    );
    assert.equal(existsSync(pidFile), false, 'startupFatal must release the newly claimed pidfile');
  },
);

test(
  'a disabled mDNS responder keeps the .local URL out of the startup banner',
  { skip: BUNDLE_SKIP },
  async (t: TestContext) => {
    // BUG-122: fleetd printed the mDNS success line immediately after start(),
    // but bind + multicast membership resolve asynchronously — with no multicast
    // route the responder disables itself one tick later and the banner (and
    // share panel) kept offering a URL that could never resolve. The banner may
    // only appear on a tick where the responder is actually alive.
    const home = freshHome('fleetdeck-mdns-banner-');
    const consoleRecord = path.join(home, 'console.log');
    t.after(() => {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });
    const daemon = spawnRaw({
      port: randomPort(),
      home,
      env: netMockEnv({
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
      if (daemon.proc.exitCode !== null)
        throw new Error(
          `daemon exited ${daemon.proc.exitCode}:\n${daemon.stdout}\n${daemon.stderr}`,
        );
      return daemon.stderr.includes('mdns disabled') || null;
    }, 'mocked mDNS responder to disable itself');

    const output = readFileSync(consoleRecord, 'utf8');
    assert.equal(
      output.includes('.local'),
      false,
      `banner advertised an unresolvable .local URL:\n${output}`,
    );
    assert.match(
      output,
      /fleetd LAN http:\/\/192\.0\.2\.77:\d+\/\?t=<hidden>/,
      'the IP URLs must still be announced',
    );
  },
);

test(
  'a live mDNS responder still gets its .local URL into the startup banner',
  { skip: BUNDLE_SKIP },
  async (t: TestContext) => {
    // The complement: gating the banner on aliveness must not silence the
    // healthy path — with the default mock (joins succeed) the line survives.
    const home = freshHome('fleetdeck-mdns-banner-ok-');
    const consoleRecord = path.join(home, 'console.log');
    t.after(() => {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });
    const daemon = spawnRaw({
      port: randomPort(),
      home,
      env: netMockEnv({
        FLEETDECK_BIND: '0.0.0.0',
        FLEETDECK_TOKEN: 'mdns-banner-token-0123456789abcdef',
        FLEETDECK_TEST_CONSOLE_RECORD: consoleRecord,
      }),
    });
    t.after(() => daemon.kill());

    await waitUntil(() => {
      if (daemon.proc.exitCode !== null)
        throw new Error(
          `daemon exited ${daemon.proc.exitCode}:\n${daemon.stdout}\n${daemon.stderr}`,
        );
      return daemon.stderr.includes('mdns responding for') || null;
    }, 'mocked mDNS responder to come up');

    const output = readFileSync(consoleRecord, 'utf8');
    assert.match(
      output,
      /fleetd LAN http:\/\/fleetdeck\.local:\d+\/\?t=<hidden> \(mDNS; credential available in share panel\)/,
      `healthy responder lost its banner line:\n${output}`,
    );
  },
);

test(
  'the mDNS responder arms unicast and multicast TTL 255 before answering',
  { skip: BUNDLE_SKIP },
  async (t: TestContext) => {
    // RFC 6762 §11: every packet an mDNS responder sends — multicast OR unicast —
    // must carry IP TTL 255, and receivers may verify it as proof the source is
    // on-link. A responder that configures only setMulticastTTL answers QU and
    // legacy queries with the platform default (commonly 64) and strict peers
    // discard them, so TTL 255 on BOTH paths is a precondition for going live.
    const home = freshHome('fleetdeck-mdns-ttl-');
    const record = path.join(home, 'mdns.jsonl');
    t.after(() => {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });

    const daemon = spawnRaw({
      port: randomPort(),
      home,
      env: netMockEnv({ FLEETDECK_BIND: '0.0.0.0', FLEETDECK_MDNS_RECORD: record }),
    });
    t.after(() => daemon.kill());

    const observed = await waitUntil(() => {
      if (daemon.proc.exitCode !== null)
        throw new Error(
          `daemon exited ${daemon.proc.exitCode}:\n${daemon.stdout}\n${daemon.stderr}`,
        );
      try {
        const records = readFileSync(record, 'utf8')
          .trim()
          .split('\n')
          .filter(Boolean)
          .map((line): MdnsLogItem => JSON.parse(line) as MdnsLogItem);
        return records.some((item) => item.type === 'send') ? records : null;
      } catch {
        return null;
      }
    }, 'mDNS TTL configuration and first announcement');

    assert.ok(
      observed.some((item) => item.type === 'setTTL' && item.value === 255),
      'unicast (QU/legacy) replies must leave with IP TTL 255, not the platform default',
    );
    assert.ok(
      observed.some((item) => item.type === 'setMulticastTTL' && item.value === 255),
      'multicast replies must leave with IP TTL 255',
    );

    // And when the platform refuses the unicast TTL, the responder must stand
    // down with an actionable log instead of answering with a non-255 TTL.
    const failHome = freshHome('fleetdeck-mdns-ttl-fail-');
    const failRecord = path.join(failHome, 'mdns.jsonl');
    t.after(() => {
      rmSync(failHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });
    const refused = spawnRaw({
      port: randomPort(),
      home: failHome,
      env: netMockEnv({
        FLEETDECK_BIND: '0.0.0.0',
        FLEETDECK_MDNS_RECORD: failRecord,
        FLEETDECK_MDNS_FAIL_TTL: 'unicast',
      }),
    });
    t.after(() => refused.kill());

    // The mDNS log goes to stderr (console.error), not the console record.
    await waitUntil(() => {
      if (refused.proc.exitCode !== null)
        throw new Error(
          `daemon exited ${refused.proc.exitCode}:\n${refused.stdout}\n${refused.stderr}`,
        );
      return refused.stderr.includes('mdns disabled');
    }, 'mDNS standing down when TTL 255 is refused');
    assert.match(refused.stderr, /mdns disabled \(cannot set unicast TTL 255\)/);
    const failRecords = readFileSync(failRecord, 'utf8')
      .trim()
      .split('\n')
      .filter(Boolean)
      .map((line): MdnsLogItem => JSON.parse(line) as MdnsLogItem);
    assert.ok(
      !failRecords.some((item) => item.type === 'send'),
      'a responder that cannot set TTL 255 must never announce',
    );
  },
);

test(
  'SIGTERM waits for the mDNS goodbye send callback before fleetd exits',
  { skip: BUNDLE_SKIP },
  async (t: TestContext) => {
    const home = freshHome('fleetdeck-goodbye-');
    const record = path.join(home, 'mdns.jsonl');
    const port = randomPort();
    t.after(() => {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });

    const child = spawnRaw({
      port,
      home,
      env: netMockEnv({
        FLEETDECK_BIND: '0.0.0.0',
        FLEETDECK_TOKEN: 'goodbye-race-token-0123456789abcdef',
        FLEETDECK_MDNS_RECORD: record,
        FLEETDECK_MDNS_SEND_DELAY_MS: '175',
      }),
    });
    t.after(() => child.kill());

    await waitUntil(() => {
      try {
        return readFileSync(record, 'utf8').includes('"type":"send"');
      } catch {
        return false;
      }
    }, 'initial mocked mDNS announcement');

    child.proc.kill('SIGTERM');
    const code = await child.waitForExit(5000);
    assert.equal(code, 0, `fleetd did not shut down cleanly:\n${child.stdout}\n${child.stderr}`);

    const records = readFileSync(record, 'utf8')
      .trim()
      .split('\n')
      .filter(Boolean)
      .map((line): MdnsLogItem => JSON.parse(line) as MdnsLogItem);
    const goodbyeSends = records.filter((item) => {
      if (item.type !== 'send') return false;
      const packet = decodeMessage(Buffer.from(item.wire ?? '', 'base64'));
      return (
        packet !== null &&
        packet.answers.length > 0 &&
        packet.answers.every((answer) => answer.ttl === 0)
      );
    });
    // BUG-130: multicast egress follows only the kernel's default route, so the
    // goodbye must be repeated out of EVERY interface the responder joined —
    // the mocked host has exactly one (192.0.2.77, via the loader's node:os
    // stub), so exactly one TTL-0 goodbye send is still the correct count here.
    // The two-interface case is covered by the injector test in tests/mdns.test.mjs.
    assert.equal(goodbyeSends.length, 1, 'signal shutdown must enqueue exactly one TTL-0 goodbye');
    const [firstGoodbye] = goodbyeSends;
    assert.ok(firstGoodbye, 'exactly one TTL-0 goodbye send was captured');
    const goodbye = decodeMessage(Buffer.from(firstGoodbye.wire ?? '', 'base64'));
    assert.ok(goodbye, 'the captured goodbye send decodes');
    assert.ok(
      goodbye.answers.some(
        (answer) =>
          answer.typeName === 'PTR' &&
          typeof answer.data === 'string' &&
          /^Fleet Deck [0-9a-f]{6}\._fleetdeck\._tcp\.local$/.test(answer.data),
      ),
      'DNS-SD instance uses a random discriminator instead of the OS hostname',
    );
    assert.ok(
      records.some((item) => item.type === 'callback' && item.wire === firstGoodbye.wire),
      'fleetd must remain alive until the goodbye send callback runs',
    );
  },
);

test(
  'a LAN address change refreshes discovery: mDNS retires the old address and announces the new one (BUG-118)',
  { skip: BUNDLE_SKIP },
  async (t: TestContext) => {
    const home = freshHome('fleetdeck-lan-roam-');
    const record = path.join(home, 'mdns.jsonl');
    const consoleRecord = path.join(home, 'console.log');
    const netFile = path.join(home, 'net.json');
    t.after(() => {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });

    const child = spawnRaw({
      port: randomPort(),
      home,
      env: netMockEnv({
        FLEETDECK_BIND: '0.0.0.0',
        FLEETDECK_TOKEN: 'lan-roam-token-0123456789abcdef',
        FLEETDECK_MDNS_RECORD: record,
        FLEETDECK_MDNS_SEND_DELAY_MS: '0',
        FLEETDECK_TEST_CONSOLE_RECORD: consoleRecord,
        FLEETDECK_TEST_NET_FILE: netFile,
        FLEETDECK_LAN_REFRESH_MS: '100',
      }),
    });
    t.after(() => child.kill());

    const packets = (): DecodedMdnsMessage[] => {
      try {
        return readFileSync(record, 'utf8')
          .trim()
          .split('\n')
          .filter(Boolean)
          .map((line): MdnsLogItem => JSON.parse(line) as MdnsLogItem)
          .filter((item) => item.type === 'send')
          .map((item) => decodeMessage(Buffer.from(item.wire ?? '', 'base64')))
          .filter((packet): packet is DecodedMdnsMessage => packet !== null);
      } catch {
        return [];
      }
    };

    // Startup announces the mocked network-A address (192.0.2.77).
    await waitUntil(() => {
      if (child.proc.exitCode !== null)
        throw new Error(`daemon exited ${child.proc.exitCode}:\n${child.stdout}\n${child.stderr}`);
      return (
        packets().some(
          (p) =>
            p.isResponse &&
            p.answers.some(
              (r) => r.typeName === 'A' && r.data === '192.0.2.77' && (r.ttl ?? 0) > 0,
            ),
        ) || null
      );
    }, 'startup announcement for network A');

    // The roam: the mocked os.networkInterfaces() starts answering 198.51.100.88.
    writeFileSync(
      netFile,
      JSON.stringify([{ family: 'IPv4', internal: false, address: '198.51.100.88' }]),
    );

    // The watcher must retire the OLD address (TTL-0 A goodbye) and announce the NEW one.
    await waitUntil(() => {
      const sent = packets();
      const goodbye = sent.some(
        (p) =>
          p.isResponse &&
          p.answers.length > 0 &&
          p.answers.every((r) => r.ttl === 0) &&
          p.answers.some((r) => r.typeName === 'A' && r.data === '192.0.2.77'),
      );
      const announced = sent.some(
        (p) =>
          p.isResponse &&
          p.answers.some(
            (r) => r.typeName === 'A' && r.data === '198.51.100.88' && (r.ttl ?? 0) > 0,
          ),
      );
      return (goodbye && announced) || null;
    }, 'mDNS goodbye for network A and announcement for network B');

    // The roam is observable in the startup log too. (refreshLan lines, recorded by
    // the daemon's recordRefreshLan seam, carry the token BY DESIGN — the share
    // panel's credentialed URLs are its whole point; the token-log contract only
    // covers the console lines, which are filtered out below.)
    const log = readFileSync(consoleRecord, 'utf8');
    const consoleLines = log
      .split('\n')
      .filter((line) => !line.startsWith('refreshLan '))
      .join('\n');
    assert.match(
      consoleLines,
      /fleetd LAN addresses now 198\.51\.100\.88/,
      `roam was not logged:\n${log}`,
    );
    assert.equal(
      consoleLines.includes('lan-roam-token-0123456789abcdef'),
      false,
      'the token must never reach a console line',
    );
    assert.match(
      log,
      /refreshLan .*198\.51\.100\.88/,
      `share-panel LAN state did not follow the roam:\n${log}`,
    );
  },
);

test(
  'a dotted FLEETDECK_MDNS_NAME yields ONE canonical host: banner, log and advertisement agree',
  { skip: BUNDLE_SKIP },
  async (t: TestContext) => {
    // BUG-120: the responder canonicalizes a raw name like "team.deck" to the
    // "team-deck.local" it can legally advertise. Anything else the daemon says
    // about the mDNS host — the startup banner and the LAN log line (and, behind
    // them, /state's lan.mdns and the Host allowlist built from it) — must name
    // that SAME host, or the share URL resolves nowhere and the advertised Host
    // is rejected with 403.
    const home = freshHome('fleetdeck-mdns-canonical-');
    const record = path.join(home, 'mdns.jsonl');
    const consoleRecord = path.join(home, 'console.log');
    t.after(() => {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });
    const daemon = spawnRaw({
      port: randomPort(),
      home,
      env: netMockEnv({
        FLEETDECK_BIND: '0.0.0.0',
        FLEETDECK_TOKEN: 'mdns-canonical-token-0123456789abcdef',
        FLEETDECK_MDNS_NAME: 'team.deck',
        FLEETDECK_MDNS_RECORD: record,
        FLEETDECK_TEST_CONSOLE_RECORD: consoleRecord,
      }),
    });
    t.after(() => daemon.kill());

    await waitUntil(() => {
      if (daemon.proc.exitCode !== null)
        throw new Error(
          `daemon exited ${daemon.proc.exitCode}:\n${daemon.stdout}\n${daemon.stderr}`,
        );
      try {
        return readFileSync(record, 'utf8').includes('"type":"send"');
      } catch {
        return false;
      }
    }, 'initial mocked mDNS announcement');

    const send = readFileSync(record, 'utf8')
      .trim()
      .split('\n')
      .filter(Boolean)
      .map((line): MdnsLogItem => JSON.parse(line) as MdnsLogItem)
      .find((item) => item.type === 'send');
    assert.ok(send, 'a mocked mDNS send record was captured');
    const packet = decodeMessage(Buffer.from(send.wire ?? '', 'base64'));
    assert.ok(packet, 'the captured send decodes');
    const aRecord = packet.answers.find((answer) => answer.typeName === 'A');
    assert.ok(aRecord, 'the announcement carries an A record');
    assert.equal(aRecord.name, 'team-deck.local', 'the wire advertises the canonicalized label');

    const output = await waitUntil(() => {
      try {
        const text = readFileSync(consoleRecord, 'utf8');
        return text.includes('(mDNS;') ? text : null;
      } catch {
        return null;
      }
    }, 'mDNS LAN log line');
    assert.match(
      output,
      /fleetd LAN http:\/\/team-deck\.local:\d+\/\?t=<hidden> \(mDNS;/,
      `the log must name the advertised host, not the raw env value:\n${output}`,
    );
    assert.ok(
      !output.includes('team.deck.local'),
      `the unsplittable raw name must never be printed:\n${output}`,
    );
  },
);

test(
  'a multihomed daemon answers and withdraws per interface, each link advertising only its own address',
  { skip: BUNDLE_SKIP },
  async (t: TestContext) => {
    // BUG-131: one socket + the kernel's multicast route means every reply leaves
    // on the OS-selected interface while the packet claims A records for BOTH
    // LANs — peers on the other link resolve an address they cannot reach. The
    // fix is one link (socket + outbound interface + scoped advertisement) per
    // interface. This test drives the daemon with two mocked interfaces and
    // asserts the wire evidence: two links, each pinned to its own interface,
    // each announcing and withdrawing only its own address.
    const home = freshHome('fleetdeck-multihome-');
    const record = path.join(home, 'mdns.jsonl');
    t.after(() => {
      rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });

    const netifs = {
      lan0: [{ family: 'IPv4', internal: false, address: '192.0.2.10' }],
      lan1: [{ family: 'IPv4', internal: false, address: '192.0.2.11' }],
    };
    const child = spawnRaw({
      port: randomPort(),
      home,
      env: netMockEnv({
        FLEETDECK_BIND: '0.0.0.0',
        FLEETDECK_TOKEN: 'multihome-race-token-0123456789abcdef',
        FLEETDECK_MDNS_RECORD: record,
        FLEETDECK_TEST_NETIFS: JSON.stringify(netifs),
      }),
    });
    t.after(() => child.kill());

    // Two pinned links means at least two setiface records — one per interface.
    await waitUntil(() => {
      try {
        const lines = readFileSync(record, 'utf8')
          .trim()
          .split('\n')
          .filter(Boolean)
          .map((line): MdnsLogItem => JSON.parse(line) as MdnsLogItem);
        return lines.filter((item) => item.type === 'setiface').length >= 2 ? true : null;
      } catch {
        return null;
      }
    }, 'both interfaces pinned for outbound multicast');

    child.proc.kill('SIGTERM');
    const code = await child.waitForExit(5000);
    assert.equal(code, 0, `fleetd did not shut down cleanly:\n${child.stdout}\n${child.stderr}`);

    const records = readFileSync(record, 'utf8')
      .trim()
      .split('\n')
      .filter(Boolean)
      .map((line): MdnsLogItem => JSON.parse(line) as MdnsLogItem);

    // Each link must pin its outbound multicast interface (setMulticastInterface)
    // and join the group through it — otherwise the kernel route decides for us.
    const setifaces = records.filter((item) => item.type === 'setiface');
    assert.deepEqual(
      new Set(setifaces.map((item) => item.address)),
      new Set(['192.0.2.10', '192.0.2.11']),
      'each interface must be selected as the outbound multicast interface for its own link',
    );

    // Every live announcement on a link advertises only that link's address; every
    // address gets a TTL-0 goodbye on its own link.
    const liveByAddress = new Map<string, number>();
    const goodbyeByAddress = new Map<string, number>();
    for (const item of records) {
      if (item.type !== 'send') continue;
      const packet = decodeMessage(Buffer.from(item.wire ?? '', 'base64'));
      if (!packet?.answers.length) continue;
      const claimed = packet.answers
        .filter((r) => r.typeName === 'A')
        .map((r) => r.data)
        .filter((d): d is string => typeof d === 'string');
      if (!claimed.length) continue;
      if (packet.answers.every((r) => r.ttl === 0)) {
        for (const a of claimed) goodbyeByAddress.set(a, (goodbyeByAddress.get(a) ?? 0) + 1);
      } else {
        for (const a of claimed) liveByAddress.set(a, (liveByAddress.get(a) ?? 0) + 1);
        assert.equal(
          claimed.length,
          1,
          `a link must advertise only its own address, got ${claimed.join(', ')}`,
        );
      }
    }
    assert.deepEqual(
      new Set(liveByAddress.keys()),
      new Set(['192.0.2.10', '192.0.2.11']),
      'both LAN addresses must be announced, each on its own link',
    );
    assert.deepEqual(
      new Set(goodbyeByAddress.keys()),
      new Set(['192.0.2.10', '192.0.2.11']),
      'both LAN addresses must be withdrawn on shutdown',
    );
  },
);
