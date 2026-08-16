// tests/mdns.test.ts
//
// The mDNS/DNS-SD responder (src/daemon/mdns.ts) is two things stacked:
// a pure DNS wire codec, and a socket that speaks it. The codec is tested here
// exhaustively and offline — the wire format is the part that is easy to get
// silently, undetectably wrong, and a browser will simply not show the board
// rather than tell you why.
//
// The socket tests need udp4 port 5353 and a multicast join. Both can be
// unavailable for legitimate reasons (a real avahi already owns the port; a
// container with no multicast), so they SKIP with a reason rather than fail —
// mDNS is a convenience the daemon must survive losing, and the test suite
// takes the same position.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import dgram from 'node:dgram';
import type { RemoteInfo, Socket } from 'node:dgram';
import {
  createMdns,
  buildAnnouncement,
  buildProbeQuestions,
  buildResponse,
  decodeMessage,
  decodeName,
  encodeMessage,
  encodeName,
  encodeRecord,
  hostLabel,
  normalize,
  parseQuestions,
  uniqueConflict,
  META_QUERY,
  MDNS_ADDR,
  MDNS_PORT,
  TYPE,
} from '../src/daemon/mdns.ts';
import { scaleMs } from './helpers/wait.ts';

// mdns.ts does not export its record/message/question shapes, so we derive them
// from the exported functions' own signatures — the types the assertions touch
// are exactly what the codec produces and consumes.
type DnsRecord = ReturnType<typeof buildAnnouncement>[number];
type DecodedMessage = NonNullable<ReturnType<typeof decodeMessage>>;
type Question = ReturnType<typeof parseQuestions>[number];
type CapturedPacket = DecodedMessage & { rinfo: RemoteInfo };
type MessageHandler = (msg: Buffer, rinfo: { address: string; port: number }) => void;

// The rdata union member for SRV — reconstructed locally to read .port/.target off
// a record whose `data` is the codec's `RecordData` union.
interface SrvData {
  priority: number;
  weight: number;
  port: number;
  target: string;
}

interface FakeSend {
  wire: string;
  iface: string | undefined;
  address: string;
}

const AD = { port: 4711, addresses: ['192.0.2.7'] }; // RFC 5737 TEST-NET-1: never routable
const HOST = 'fleetdeck.local';
const SVC = '_fleetdeck._tcp.local';
const HTTP_SVC = '_http._tcp.local';
const INSTANCE = 'Fleet Deck._fleetdeck._tcp.local';
const TWO_IFACES = { port: 4711, addresses: ['192.0.2.7', '192.0.2.8'] };

const only = (records: DnsRecord[], type: string): DnsRecord[] =>
  records.filter((r) => r.type === type || r.typeName === type);

// buildResponse takes a full Question[] (typeName + unicast are required); the wire
// tests only care about name/type/class, so this fills the rest with inert values.
const mkQuestion = (name: string, type: number, cls = 1): Question => ({
  name,
  type,
  typeName: '',
  class: cls,
  unicast: false,
});

// decodeMessage returns `DecodedMessage | null`; every codec assertion below wants
// the message, so fail loudly here rather than propagate a null.
const decode = (buf: Buffer): DecodedMessage => {
  const msg = decodeMessage(buf);
  assert.ok(msg, 'expected a decodable DNS message');
  return msg;
};

const delay = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(() => {
      resolve();
    }, ms);
  });

// A skip/diagnostic string for a caught error: the errno code when there is one,
// else the message, else a stringified non-Error.
const errText = (err: unknown): string =>
  err instanceof Error ? ((err as NodeJS.ErrnoException).code ?? err.message) : String(err);

// ------------------------------------------------------------- the codec

test('encodeName/decodeName round-trip a dotted name', () => {
  const buf = encodeName(HOST);
  // length-prefixed labels, root NUL: 9 'fleetdeck' 5 'local' 0
  assert.deepEqual(
    buf,
    Buffer.from([9, ...Buffer.from('fleetdeck'), 5, ...Buffer.from('local'), 0]),
  );
  assert.deepEqual(decodeName(buf, 0), { name: HOST, offset: buf.length });
});

test('encodeName keeps a space inside a DNS-SD instance label', () => {
  // "Fleet Deck" is ONE label containing a space (RFC 6763 §4.1.1) — not two.
  const { name, offset } = decodeName(encodeName(INSTANCE), 0);
  assert.equal(name, INSTANCE);
  assert.equal(offset, encodeName(INSTANCE).length);
  assert.equal(encodeName(INSTANCE)[0], 'Fleet Deck'.length, 'first label spans the space');
});

test('decodeName follows a compression pointer and reports the offset after the POINTER', () => {
  // Hand-built: "fleetdeck.local" at 0, then "_http._tcp" + a pointer back to
  // the "local" label inside it. Real resolvers emit exactly this shape.
  const base = encodeName(HOST); // [0..16]
  const localAt = base.indexOf(5); // offset of the "local" label
  const tail = Buffer.concat([
    Buffer.from([5, ...Buffer.from('_http'), 4, ...Buffer.from('_tcp')]),
    Buffer.from([0xc0 | ((localAt >> 8) & 0x3f), localAt & 0xff]),
  ]);
  const packet = Buffer.concat([base, tail]);

  const decoded = decodeName(packet, base.length);
  assert.equal(decoded.name, '_http._tcp.local', 'the pointer must expand to the shared suffix');
  assert.equal(
    decoded.offset,
    packet.length,
    'offset lands after the 2 pointer bytes, not after its target',
  );
});

test('decodeName refuses a pointer loop instead of spinning forever', () => {
  const loop = Buffer.from([0xc0, 0x00]); // a pointer to itself
  assert.throws(() => decodeName(loop, 0), /loop/);
});

test('parseQuestions reads a real-looking query, including the QU bit and a compression pointer', () => {
  // Two questions: A for fleetdeck.local (QU set), then PTR for _fleetdeck._tcp
  // whose ".local" is a pointer back into the first question's name.
  const header = Buffer.alloc(12);
  header.writeUInt16BE(0x1234, 0);
  header.writeUInt16BE(0x0000, 2); // a query: QR clear
  header.writeUInt16BE(2, 4); // QDCOUNT

  const q1name = encodeName(HOST);
  const q1 = Buffer.alloc(4);
  q1.writeUInt16BE(TYPE.A, 0);
  q1.writeUInt16BE(0x8001, 2); // QU bit + class IN

  const localAt = 12 + q1name.indexOf(5);
  const q2name = Buffer.concat([
    Buffer.from([10, ...Buffer.from('_fleetdeck'), 4, ...Buffer.from('_tcp')]),
    Buffer.from([0xc0 | ((localAt >> 8) & 0x3f), localAt & 0xff]),
  ]);
  const q2 = Buffer.alloc(4);
  q2.writeUInt16BE(TYPE.PTR, 0);
  q2.writeUInt16BE(0x0001, 2); // no QU bit

  const questions = parseQuestions(Buffer.concat([header, q1name, q1, q2name, q2]));
  assert.deepEqual(questions, [
    { name: HOST, type: TYPE.A, typeName: 'A', class: 1, unicast: true },
    { name: SVC, type: TYPE.PTR, typeName: 'PTR', class: 1, unicast: false },
  ]);
});

test('parseQuestions returns what it understood from a truncated packet, and never throws', () => {
  const header = Buffer.alloc(12);
  header.writeUInt16BE(3, 4); // claims 3 questions
  const truncated = Buffer.concat([header, encodeName(HOST), Buffer.from([0x00])]); // qtype cut in half
  assert.deepEqual(parseQuestions(truncated), []);
  assert.deepEqual(parseQuestions(Buffer.alloc(4)), [], 'a runt packet is not a crash');
});

test('encodeRecord sets the cache-flush bit only when asked, and encodes each rdata type', () => {
  const a = decode(
    encodeMessage({
      answers: [{ name: HOST, type: 'A', ttl: 120, flush: true, data: '192.0.2.7' }],
    }),
  );
  const aRec = a.answers[0];
  assert.ok(aRec);
  assert.equal(aRec.data, '192.0.2.7');
  assert.equal(aRec.flush, true);
  assert.equal(aRec.class, 1, 'the flush bit must not leak into the class');

  const srv = decode(
    encodeMessage({
      answers: [
        {
          name: INSTANCE,
          type: 'SRV',
          ttl: 120,
          flush: true,
          data: { priority: 0, weight: 0, port: 4711, target: HOST },
        },
      ],
    }),
  );
  assert.deepEqual(srv.answers[0]?.data, { priority: 0, weight: 0, port: 4711, target: HOST });

  const txt = decode(
    encodeMessage({
      answers: [
        {
          name: INSTANCE,
          type: 'TXT',
          ttl: 4500,
          flush: false,
          data: { path: '/', board: 'fleetdeck' },
        },
      ],
    }),
  );
  const txtRec = txt.answers[0];
  assert.ok(txtRec);
  assert.deepEqual(txtRec.data, ['path=/', 'board=fleetdeck']);
  assert.equal(txtRec.flush, false);

  const ptr = decode(
    encodeMessage({
      answers: [{ name: SVC, type: 'PTR', ttl: 4500, flush: false, data: INSTANCE }],
    }),
  );
  assert.equal(ptr.answers[0]?.data, INSTANCE);

  // A record we do not own must not be silently encoded as garbage.
  assert.throws(() => encodeRecord({ name: HOST, type: 'AAAA', ttl: 120, data: '::1' }), /rdata/);
});

// -------------------------------------------------------- what we answer

test('buildResponse answers an A query for fleetdeck.local with every advertised address', () => {
  const { answers, additionals } = buildResponse([mkQuestion(HOST, TYPE.A)], {
    port: 4711,
    addresses: ['192.0.2.7', '192.0.2.8'],
  });

  assert.equal(answers.length, 2);
  assert.deepEqual(
    answers.map((r) => r.data),
    ['192.0.2.7', '192.0.2.8'],
  );
  for (const r of answers) {
    assert.equal(r.name, HOST);
    assert.equal(r.type, 'A');
    assert.equal(r.ttl, 120, 'RFC 6762 §10: a host record gets the 120s TTL');
    assert.equal(r.flush, true, 'we uniquely own our own A record');
  }
  assert.equal(additionals.length, 0, 'an A query needs no extras');
});

test('buildResponse matches names case-insensitively and answers ANY', () => {
  const upper = buildResponse([mkQuestion('FleetDeck.LOCAL', TYPE.A)], AD);
  assert.equal(upper.answers.length, 1, 'RFC 6762 §16: names compare case-insensitively');

  const any = buildResponse([mkQuestion(HOST, TYPE.ANY)], AD);
  assert.equal(any.answers.length, 1);
  assert.equal(any.answers[0]?.type, 'A');
});

test("buildResponse stays silent for AAAA, for a stranger's name, and for a service that is not ours", () => {
  for (const q of [
    mkQuestion(HOST, TYPE.AAAA),
    mkQuestion('someone-else.local', TYPE.A),
    mkQuestion('_ssh._tcp.local', TYPE.PTR),
    mkQuestion(HOST, TYPE.A, 3), // not class IN
  ]) {
    const { answers } = buildResponse([q], AD);
    assert.equal(answers.length, 0, `must not answer ${q.name}/${q.type}/class ${q.class}`);
  }
});

test('buildResponse answers a PTR browse with SRV + TXT + A in the ADDITIONAL section', () => {
  // THE one that matters. RFC 6763 §12.1: without these extras a browser shows a
  // name it cannot connect to, and needs two more round-trips to fix that.
  const { answers, additionals } = buildResponse([mkQuestion(SVC, TYPE.PTR)], AD);

  assert.equal(answers.length, 1);
  const ptr = answers[0];
  assert.ok(ptr);
  assert.equal(ptr.name, SVC);
  assert.equal(ptr.type, 'PTR');
  assert.equal(ptr.data, INSTANCE);
  assert.equal(ptr.ttl, 4500);
  assert.equal(
    ptr.flush,
    false,
    'PTR is a SHARED record set — flushing it would evict our neighbours',
  );

  const srv = only(additionals, 'SRV');
  const txt = only(additionals, 'TXT');
  const a = only(additionals, 'A');
  assert.equal(srv.length, 1, 'SRV must ride along');
  assert.equal(txt.length, 1, 'TXT must ride along');
  assert.equal(a.length, 1, 'the A record for the SRV target must ride along');

  const srv0 = srv[0];
  const txt0 = txt[0];
  const a0 = a[0];
  assert.ok(srv0 && txt0 && a0);
  assert.equal(srv0.name, INSTANCE);
  assert.deepEqual(srv0.data, { priority: 0, weight: 0, port: 4711, target: HOST });
  assert.equal((txt0.data as Record<string, string>)['path'], '/');
  assert.equal((txt0.data as Record<string, string>)['board'], 'fleetdeck');
  assert.equal(a0.name, HOST);
  assert.equal(a0.data, '192.0.2.7');
});

test('buildResponse answers a PTR browse for the generic _http._tcp type too', () => {
  const { answers, additionals } = buildResponse([mkQuestion(HTTP_SVC, TYPE.PTR)], AD);
  assert.equal(answers.length, 1);
  assert.equal(answers[0]?.data, `Fleet Deck.${HTTP_SVC}`);
  const httpSrv = only(additionals, 'SRV')[0];
  assert.ok(httpSrv);
  assert.deepEqual((httpSrv.data as SrvData).port, 4711);
  assert.equal(only(additionals, 'A').length, 1);
});

test('buildResponse answers the _services._dns-sd._udp meta-query with both service types', () => {
  const { answers } = buildResponse([mkQuestion(META_QUERY, TYPE.PTR)], AD);
  assert.deepEqual(answers.map((r) => r.data).sort(), [HTTP_SVC, SVC].sort());
  for (const r of answers) {
    assert.equal(r.name, META_QUERY);
    assert.equal(r.type, 'PTR');
    assert.equal(r.flush, false);
  }
});

test('buildResponse resolves the instance name: SRV carries the A record, TXT stands alone', () => {
  const srv = buildResponse([mkQuestion(INSTANCE, TYPE.SRV)], AD);
  assert.equal(srv.answers.length, 1);
  assert.equal(srv.answers[0]?.type, 'SRV');
  assert.equal(
    only(srv.additionals, 'A').length,
    1,
    'RFC 6763 §12.2: the SRV target needs its address',
  );

  const txt = buildResponse([mkQuestion(INSTANCE, TYPE.TXT)], AD);
  assert.equal(txt.answers.length, 1);
  assert.equal(txt.answers[0]?.type, 'TXT');
});

test('buildResponse de-duplicates across questions and never repeats an answer as an additional', () => {
  const { answers, additionals } = buildResponse(
    [
      mkQuestion(SVC, TYPE.PTR),
      mkQuestion(HOST, TYPE.A), // the A is also a PTR additional
      mkQuestion(SVC, TYPE.PTR), // asked twice
    ],
    AD,
  );

  assert.equal(only(answers, 'PTR').length, 1, 'the PTR is answered once');
  assert.equal(only(answers, 'A').length, 1);
  assert.equal(
    only(additionals, 'A').length,
    0,
    'an A already in ANSWER must not be repeated in ADDITIONAL',
  );
});

test('buildResponse with no advertisable address answers nothing at all', () => {
  const { answers } = buildResponse([mkQuestion(HOST, TYPE.A)], { port: 4711, addresses: [] });
  assert.equal(answers.length, 0, 'better silent than pointing at a host with no address');
});

test('normalize drops junk addresses and turns a dotted instance into one legal label', () => {
  const ad = normalize({
    port: 4711,
    addresses: ['192.0.2.7', '::1', '999.1.1.1', 'nonsense'],
    instance: 'Luis. Deck',
  });
  assert.deepEqual(ad.addresses, ['192.0.2.7'], 'only real IPv4 goes in an A record');
  assert.ok(!ad.instance.includes('.'), 'a dot in an instance name would split it into two labels');
  assert.equal(ad.host, HOST);
});

test('hostLabel returns exactly the label normalize() would advertise', () => {
  // BUG-119: fleetd builds its share URL, startup log line and Host allowlist
  // from ONE canonical label — if the surfaces and the advertisement ever
  // diverge, the published name does not resolve and the advertised name is
  // refused by the daemon's own Host wall.
  for (const raw of [
    'deck.office',
    'deck office',
    ' spaced ',
    'x'.repeat(80),
    'café.'.repeat(20),
  ]) {
    const canonical = hostLabel(raw);
    assert.equal(
      normalize({ port: 4711, addresses: ['192.0.2.7'], name: raw }).host,
      `${canonical}.local`,
    );
    assert.ok(
      !canonical.includes('.'),
      'a dot in the advertised host would split it into two labels',
    );
    assert.ok(Buffer.byteLength(canonical, 'utf8') <= 63, 'a DNS label never exceeds 63 bytes');
  }
  assert.equal(hostLabel('deck.office'), 'deck-office');
  assert.equal(
    hostLabel(''),
    'fleetdeck',
    'an empty name falls back rather than advertising ".local"',
  );
});

// BUG-120: the daemon builds its share URL, Host allowlist and logs from
// hostLabel(), so those must agree byte-for-byte with the host normalize()
// actually advertises — for every shape of FLEETDECK_MDNS_NAME.
test('hostLabel is the same canonical host normalize() advertises', () => {
  const cases: [string, string][] = [
    ['team.deck', 'team-deck'], // a dot would split one label into two
    ['teamdeck', 'team-deck'], // control bytes never reach the wire
    ['  spaced  ', 'spaced'], // trim, like the daemon already did
    ['é'.repeat(40), 'é'.repeat(31)], // byte-wise truncation drops a half-eaten tail
    ['', 'fleetdeck'], // empty falls back
  ];
  for (const [raw, expected] of cases) {
    const canonical = hostLabel(raw || 'fleetdeck', 'fleetdeck');
    assert.equal(canonical, expected, `hostLabel(${JSON.stringify(raw)})`);
    assert.equal(
      normalize({ name: canonical, port: 4711 }).host,
      `${canonical}.local`,
      `the advertised host must be built from the same label: ${JSON.stringify(raw)}`,
    );
  }
});

// ------------------------------------------------- announcements & goodbyes

test('buildAnnouncement carries the whole advertisement in one Answer section', () => {
  const records = buildAnnouncement(AD);
  const names = records.map((r) => `${r.typeName ?? r.type} ${r.name}`);

  assert.deepEqual(
    new Set(names),
    new Set([
      `A ${HOST}`,
      `PTR ${META_QUERY}`, // once per service type — the Set collapses them
      `PTR ${SVC}`,
      `SRV Fleet Deck.${SVC}`,
      `TXT Fleet Deck.${SVC}`,
      `PTR ${HTTP_SVC}`,
      `SRV Fleet Deck.${HTTP_SVC}`,
      `TXT Fleet Deck.${HTTP_SVC}`,
    ]),
  );
  assert.equal(
    only(records, 'PTR').filter((r) => r.name === META_QUERY).length,
    2,
    'both types listed under the meta-query',
  );
  assert.ok(
    records.every((r) => (r.ttl ?? 0) > 0),
    'a live announcement never has a zero TTL',
  );
});

test('a goodbye is the same record set with TTL 0 and no cache-flush claim', () => {
  const alive = buildAnnouncement(AD);
  const goodbye = buildAnnouncement(AD, { ttl: 0 });

  assert.equal(goodbye.length, alive.length, 'we withdraw exactly what we claimed');
  assert.ok(
    goodbye.every((r) => r.ttl === 0),
    'RFC 6762 §10.1: a goodbye is TTL 0',
  );
  assert.ok(
    goodbye.every((r) => r.flush === false),
    'a record being withdrawn must not also claim uniqueness',
  );
});

// ------------------------------------------------------------- update()

// BUG-129: a network change (Wi-Fi roam, DHCP renewal, VPN up/down) must NOT
// leave the responder advertising the startup snapshot for the daemon's whole
// lifetime. update() re-bases the advertisement on the address set the host has
// NOW: withdrawn records get a TTL-0 goodbye, the fresh set is announced.

test('update() re-bases the advertisement: queries answer with the NEW addresses', async (t) => {
  const probe = await bindShared(MDNS_PORT);
  if (!probe) {
    t.skip('udp4 port 5353 is already owned by another responder');
    return;
  }
  await close(probe);
  if (await foreignResponderOn5353()) {
    t.skip('another responder shares udp/5353 — unicast delivery is ambiguous in this environment');
    return;
  }

  const logs: string[] = [];
  const mdns = createMdns({
    port: 4711,
    addresses: ['192.0.2.7'],
    log: (m) => {
      logs.push(m);
    },
  });
  mdns.start();
  t.after(() => mdns.stop());
  await delay(scaleMs(250));
  const disabled = logs.find((m) => m.includes('mdns disabled'));
  if (disabled !== undefined) {
    t.skip(`responder degraded to a no-op in this environment: ${disabled}`);
    return;
  }

  // The DHCP lease hands us a new address: the old one is gone.
  mdns.update({ addresses: ['198.51.100.9'] }); // RFC 5737 TEST-NET-2

  const asker = await bindShared(0);
  assert.ok(asker, 'the test needs an ephemeral udp4 socket');
  t.after(() => close(asker));
  const inbox = collect(asker);
  asker.send(
    encodeMessage({ id: 2, flags: 0, questions: [{ name: HOST, type: TYPE.A, class: 1 }] }),
    MDNS_PORT,
    '127.0.0.1',
  );

  const reply = await inbox.waitFor(
    (p) => p.isResponse && p.answers.some((r) => r.typeName === 'A'),
    'the A answer after update',
  );
  assert.ok(reply, `no mDNS response arrived after update(). logs: ${JSON.stringify(logs)}`);
  assert.deepEqual(
    reply.answers.filter((r) => r.typeName === 'A').map((r) => r.data),
    ['198.51.100.9'],
    'the responder must answer with the new address, not the startup snapshot',
  );
});

test('update() goodbyes the removed addresses and announces the new set', async (t) => {
  const listener = await bindShared(MDNS_PORT);
  if (!listener) {
    t.skip('udp4 port 5353 is already owned by another responder');
    return;
  }
  t.after(() => close(listener));
  try {
    listener.addMembership(MDNS_ADDR);
  } catch (err) {
    t.skip(`cannot join ${MDNS_ADDR} in this environment (${errText(err)})`);
    return;
  }
  const inbox = collect(listener);

  const logs: string[] = [];
  const mdns = createMdns({
    port: 4711,
    addresses: ['192.0.2.7'],
    log: (m) => {
      logs.push(m);
    },
  });
  mdns.start();
  t.after(() => mdns.stop());

  const opening = await inbox.waitFor(
    (p) => p.isResponse && p.answers.some((r) => (r.ttl ?? 0) > 0),
    'an announcement',
  );
  if (!opening) {
    t.skip(
      `multicast loopback does not deliver to this host — cannot observe goodbyes. logs: ${JSON.stringify(logs)}`,
    );
    return;
  }
  inbox.packets.length = 0;

  mdns.update({ addresses: ['198.51.100.9'] });

  const goodbye = await inbox.waitFor(
    (p) => p.isResponse && p.answers.length > 0 && p.answers.every((r) => r.ttl === 0),
    'the goodbye',
  );
  assert.ok(goodbye, 'a removed address must be withdrawn with a TTL-0 goodbye');
  assert.ok(
    goodbye.answers.some((r) => r.typeName === 'A' && r.data === '192.0.2.7'),
    'the goodbye must name the OLD address, so peer caches drop the stale route',
  );
  assert.ok(
    !goodbye.answers.some((r) => r.typeName === 'A' && r.data === '198.51.100.9'),
    'a goodbye must not withdraw the address we still own',
  );

  const announce = await inbox.waitFor(
    (p) => p.isResponse && p.answers.some((r) => r.typeName === 'A' && (r.ttl ?? 0) > 0),
    'the re-announcement',
  );
  assert.ok(announce, 'the new address set must be announced');
  assert.ok(
    announce.answers.some((r) => r.typeName === 'A' && r.data === '198.51.100.9'),
    'the announcement must carry the NEW address',
  );
});

test('update() after stop() and with nothing to change is a quiet no-op', async () => {
  const logs: string[] = [];
  const mdns = createMdns({
    port: 4711,
    addresses: ['192.0.2.7'],
    log: (m) => {
      logs.push(m);
    },
  });
  assert.doesNotThrow(
    () => mdns.update({ addresses: ['198.51.100.9'] }),
    'update() before start() must not throw or bind',
  );
  await assert.doesNotReject(async () => {
    await mdns.stop();
  });
  assert.doesNotThrow(
    () => mdns.update({ addresses: ['198.51.100.9'] }),
    'update() must not resurrect a stopped responder',
  );
  assert.equal(logs.length, 0, 'a no-op update says nothing');
});

// ------------------------------------------------- multicast egress (BUG-130)
//
// One socket joins the group on every advertised interface, but multicast
// egress follows only the kernel's DEFAULT route — so announcements, multicast
// replies and the goodbye must be repeated once per joined interface via
// setMulticastInterface, or a dual-homed host is discoverable on one LAN and
// invisible on the other. These use the injector so the "two interfaces" are
// two fake sockets; no network is touched.

/** A fake node:dgram with two interfaces, recording sends per interface. */
function fakeDgramTwoIfaces(): { sends: FakeSend[]; dgram: typeof dgram } {
  const sends: FakeSend[] = [];
  let iface: string | undefined;
  const socket = {
    on() {
      return this;
    },
    setMulticastTTL() {
      /* no-op */
    },
    setMulticastLoopback() {
      /* no-op */
    },
    setMulticastInterface(address: string) {
      iface = address;
    },
    addMembership() {
      /* no-op */
    },
    bind(_options: unknown, callback: () => void) {
      setImmediate(callback);
      return this;
    },
    send(
      packet: Buffer,
      _port: number,
      address: string,
      callback: () => void = () => {
        /* no-op */
      },
    ) {
      sends.push({ wire: Buffer.from(packet).toString('base64'), iface, address });
      setImmediate(callback);
      return this;
    },
    close(callback?: () => void) {
      setImmediate(() => {
        callback?.();
      });
    },
  };
  return { sends, dgram: { createSocket: () => socket } as unknown as typeof dgram };
}

test('announcements and goodbyes leave through EVERY joined interface, not just the default route', async () => {
  const { sends, dgram: fakeDgram } = fakeDgramTwoIfaces();
  const mdns = createMdns({ ...TWO_IFACES, inject: { dgram: fakeDgram } });
  mdns.start();
  await delay(30); // bind + joins + first (delay 0) announcement
  await mdns.stop();

  const multicast = sends.filter((s) => s.address === MDNS_ADDR);
  const live = multicast.filter((s) =>
    decode(Buffer.from(s.wire, 'base64')).answers.some((r) => (r.ttl ?? 0) > 0),
  );
  const goodbye = multicast.filter((s) => {
    const answers = decode(Buffer.from(s.wire, 'base64')).answers;
    return answers.length > 0 && answers.every((r) => r.ttl === 0);
  });

  for (const address of TWO_IFACES.addresses) {
    assert.ok(
      live.some((s) => s.iface === address),
      `an announcement must egress via ${address}`,
    );
    assert.ok(
      goodbye.some((s) => s.iface === address),
      `the goodbye must egress via ${address}`,
    );
  }
  assert.equal(goodbye.length, 2, 'one goodbye per joined interface');
});

test('a multicast query gets its reply on every joined interface; a unicast (QU) reply goes once, to the querier', async () => {
  const sends: FakeSend[] = [];
  let iface: string | undefined;
  let handler: MessageHandler | undefined;
  const socket = {
    on(event: string, fn: MessageHandler) {
      if (event === 'message') handler = fn;
      return this;
    },
    setMulticastTTL() {
      /* no-op */
    },
    setMulticastLoopback() {
      /* no-op */
    },
    setMulticastInterface(address: string) {
      iface = address;
    },
    addMembership() {
      /* no-op */
    },
    bind(_options: unknown, callback: () => void) {
      setImmediate(callback);
      return this;
    },
    send(
      packet: Buffer,
      _port: number,
      address: string,
      callback: () => void = () => {
        /* no-op */
      },
    ) {
      // setMulticastInterface only steers MULTICAST egress; unicast follows the
      // routing table, so a unicast send is recorded with no multicast iface.
      sends.push({
        wire: Buffer.from(packet).toString('base64'),
        iface: address === MDNS_ADDR ? iface : undefined,
        address,
      });
      setImmediate(callback);
      return this;
    },
    close(callback?: () => void) {
      setImmediate(() => {
        callback?.();
      });
    },
  };
  const fakeDgram = { createSocket: () => socket } as unknown as typeof dgram;

  const mdns = createMdns({ ...TWO_IFACES, inject: { dgram: fakeDgram } });
  mdns.start();
  await delay(30);
  sends.length = 0;

  assert.ok(handler, 'the responder must register a message handler');
  const dispatch = handler;

  // A QU query from the same multicast source port: one unicast reply, no fan-out.
  dispatch(
    encodeMessage({
      id: 0,
      flags: 0,
      questions: [{ name: HOST, type: TYPE.A, class: 1, unicast: true }],
    }),
    { address: '192.0.2.200', port: MDNS_PORT },
  );
  await delay(20);
  assert.equal(sends.length, 1, 'a unicast reply must not fan out across interfaces');
  const unicast = sends[0];
  assert.ok(unicast);
  assert.equal(unicast.address, '192.0.2.200');
  assert.equal(
    unicast.iface,
    undefined,
    'a unicast reply must not be attributed to a multicast interface',
  );

  sends.length = 0;
  // An ordinary multicast query (source port 5353, no QU bit).
  dispatch(
    encodeMessage({ id: 0, flags: 0, questions: [{ name: HOST, type: TYPE.A, class: 1 }] }),
    { address: '192.0.2.200', port: MDNS_PORT },
  );
  await delay(20);
  const replies = sends.filter((s) => s.address === MDNS_ADDR);
  assert.equal(replies.length, 2, 'the multicast answer must be repeated per interface');
  assert.deepEqual(new Set(replies.map((s) => s.iface)), new Set(TWO_IFACES.addresses));

  await mdns.stop();
});

// ------------------------------------------------------------ the socket

/** Bind a udp4 socket, resolving null (rather than throwing) if 5353 is taken. */
function bindShared(port: number): Promise<Socket | null> {
  return new Promise((resolve) => {
    const socket = dgram.createSocket({ type: 'udp4', reuseAddr: true });
    socket.once('error', () => {
      try {
        socket.close();
      } catch {
        /* already gone */
      }
      resolve(null);
    });
    socket.bind({ port }, () => {
      resolve(socket);
    });
  });
}

function collect(socket: Socket) {
  const packets: CapturedPacket[] = [];
  socket.on('message', (msg: Buffer, rinfo: RemoteInfo) => {
    const decoded = decodeMessage(msg);
    if (decoded) packets.push({ ...decoded, rinfo });
  });
  return {
    packets,
    async waitFor(
      predicate: (p: CapturedPacket) => boolean,
      _label: string,
      timeoutMs = 4000,
    ): Promise<CapturedPacket | null> {
      const deadline = Date.now() + scaleMs(timeoutMs);
      for (;;) {
        const hit = packets.find(predicate);
        if (hit) return hit;
        if (Date.now() >= deadline) return null;
        await delay(25);
      }
    },
  };
}

// A `function` (not a const arrow) so it hoists alongside bindShared/collect and
// can be referenced by the tests above its definition — behaviour is identical.
function close(socket: Socket): Promise<void> {
  return new Promise((resolve) => {
    try {
      socket.close(() => {
        resolve();
      });
    } catch {
      resolve();
    }
  });
}

/**
 * Empirically decide whether a unicast datagram to 127.0.0.1:5353 actually
 * reaches OUR shared-bound socket. bindShared(5353) succeeding is not enough:
 * on macOS, Bonjour's mDNSResponder holds 5353 with SO_REUSEPORT-style sharing,
 * so our bind succeeds but the kernel may hand the datagram to mDNSResponder's
 * socket instead — the legacy-unicast-query tests below would then wait forever
 * for a reply that went elsewhere. So we send ourselves one junk datagram and
 * check whether we get it back. Returns true when it did NOT arrive (another
 * process on 5353 swallowed it), meaning unicast delivery is ambiguous here.
 */
async function foreignResponderOn5353(): Promise<boolean> {
  const probe = await bindShared(MDNS_PORT);
  if (!probe) return true; // can't even share the port — treat it as foreign-owned
  const sender = await bindShared(0);
  if (!sender) {
    await close(probe);
    return true;
  } // no ephemeral socket to probe with — same conclusion

  let retry: ReturnType<typeof setTimeout> | undefined;
  let deadline: ReturnType<typeof setTimeout> | undefined;
  const arrived = await new Promise<boolean>((resolve) => {
    const done = (v: boolean): void => {
      if (retry) clearTimeout(retry);
      if (deadline) clearTimeout(deadline);
      resolve(v);
    };
    probe.once('message', () => {
      done(true);
    }); // OUR socket received it — unicast delivery works here
    const shoot = (): void => {
      try {
        sender.send(Buffer.from([0]), MDNS_PORT, '127.0.0.1');
      } catch {
        /* the point is whether it lands */
      }
    };
    shoot(); // probe is already bound+listening: bindShared resolves in the bind callback
    retry = setTimeout(shoot, scaleMs(200)); // re-send once, in case scheduler lag beat the first send to the listener
    deadline = setTimeout(() => {
      done(false);
    }, scaleMs(600));
  });

  await close(sender);
  await close(probe);
  return !arrived; // datagram vanished => another responder on 5353 owns it
}

test('a real A query on the wire gets a real answer carrying the advertised IPv4', async (t) => {
  // The responder needs udp4/5353. If a real avahi owns it, standing down is the
  // CORRECT behaviour, not a failure — so skip rather than fail.
  const probe = await bindShared(MDNS_PORT);
  if (!probe) {
    t.skip(
      'udp4 port 5353 is already owned by another responder (avahi/Bonjour?) — nothing to test',
    );
    return;
  }
  await close(probe);
  // Sharing the port can succeed while a co-bound responder (macOS Bonjour) still
  // wins our unicast datagrams. If our own probe cannot reach us, standing down is
  // correct — the legacy-query reply below would land in someone else's socket.
  if (await foreignResponderOn5353()) {
    t.skip(
      'another responder shares udp/5353 (mDNSResponder/Bonjour?) — unicast delivery is ambiguous in this environment',
    );
    return;
  }

  const logs: string[] = [];
  const mdns = createMdns({
    port: 4711,
    addresses: ['192.0.2.7'],
    log: (m) => {
      logs.push(m);
    },
  });
  mdns.start();
  t.after(() => mdns.stop());

  // start() is async under the hood (bind + join); give it a beat, then check it
  // did not degrade. No multicast in this environment is a skip, not a failure.
  await delay(scaleMs(250));
  const disabled = logs.find((m) => m.includes('mdns disabled'));
  if (disabled !== undefined) {
    t.skip(`responder degraded to a no-op in this environment: ${disabled}`);
    return;
  }

  const asker = await bindShared(0); // an ephemeral port => a LEGACY unicast query
  assert.ok(asker, 'the test needs an ephemeral udp4 socket');
  t.after(() => close(asker));
  const inbox = collect(asker);

  const query = encodeMessage({
    id: 0xbeef,
    flags: 0, // QR clear: this is a question
    questions: [{ name: HOST, type: TYPE.A, class: 1 }],
  });
  asker.send(query, MDNS_PORT, '127.0.0.1');

  const reply = await inbox.waitFor(
    (p) => p.isResponse && p.answers.some((r) => r.typeName === 'A'),
    'the A answer',
  );
  assert.ok(reply, `no mDNS response arrived for ${HOST}. logs: ${JSON.stringify(logs)}`);

  const a = reply.answers.find((r) => r.typeName === 'A');
  assert.ok(a);
  assert.equal(a.name, HOST);
  assert.equal(a.data, '192.0.2.7', 'the advertised LAN address must come back on the wire');

  // RFC 6762 §6.7 — a legacy (non-5353 source port) resolver matches on the echoed
  // ID and question, and must get a short TTL with no cache-flush bit.
  assert.equal(reply.id, 0xbeef, 'a legacy query`s ID must be echoed');
  assert.deepEqual(
    reply.questions.map((q) => q.name),
    [HOST],
    'a legacy query`s question must be echoed',
  );
  assert.equal(a.ttl, 10);
  assert.equal(a.flush, false);
});

test('a PTR browse on the wire resolves the board in one round-trip', async (t) => {
  const probe = await bindShared(MDNS_PORT);
  if (!probe) {
    t.skip('udp4 port 5353 is already owned by another responder');
    return;
  }
  await close(probe);
  if (await foreignResponderOn5353()) {
    t.skip(
      'another responder shares udp/5353 (mDNSResponder/Bonjour?) — unicast delivery is ambiguous in this environment',
    );
    return;
  }

  const logs: string[] = [];
  const mdns = createMdns({
    port: 4711,
    addresses: ['192.0.2.7'],
    log: (m) => {
      logs.push(m);
    },
  });
  mdns.start();
  t.after(() => mdns.stop());
  await delay(scaleMs(250));
  const disabled = logs.find((m) => m.includes('mdns disabled'));
  if (disabled !== undefined) {
    t.skip(`responder degraded to a no-op: ${disabled}`);
    return;
  }

  const asker = await bindShared(0);
  assert.ok(asker, 'the test needs an ephemeral udp4 socket');
  t.after(() => close(asker));
  const inbox = collect(asker);

  asker.send(
    encodeMessage({ id: 1, flags: 0, questions: [{ name: SVC, type: TYPE.PTR, class: 1 }] }),
    MDNS_PORT,
    '127.0.0.1',
  );

  const reply = await inbox.waitFor(
    (p) => p.isResponse && p.answers.some((r) => r.typeName === 'PTR'),
    'the PTR answer',
  );
  assert.ok(reply, `no PTR response arrived. logs: ${JSON.stringify(logs)}`);

  assert.equal(reply.answers[0]?.data, INSTANCE);
  // Everything a browser needs to open a socket, without asking a second question.
  const srv = reply.additionals.find((r) => r.typeName === 'SRV');
  const a = reply.additionals.find((r) => r.typeName === 'A');
  const txt = reply.additionals.find((r) => r.typeName === 'TXT');
  assert.ok(srv && a && txt, 'SRV + A + TXT must all be in the additional section');
  assert.equal((srv.data as SrvData).port, 4711);
  assert.equal((srv.data as SrvData).target, HOST);
  assert.equal(a.data, '192.0.2.7');
  assert.ok((txt.data as string[]).includes('path=/'));
});

test('stop() puts goodbye records (TTL 0) on the multicast group', async (t) => {
  // This one genuinely needs multicast loopback: a goodbye has no question to
  // answer, so it only ever goes to 224.0.0.251. Skip if the kernel/network does
  // not loop our own multicast back to us.
  const listener = await bindShared(MDNS_PORT);
  if (!listener) {
    t.skip('udp4 port 5353 is already owned by another responder');
    return;
  }
  t.after(() => close(listener));

  try {
    listener.addMembership(MDNS_ADDR);
  } catch (err) {
    t.skip(`cannot join ${MDNS_ADDR} in this environment (${errText(err)})`);
    return;
  }
  const inbox = collect(listener);

  const logs: string[] = [];
  const mdns = createMdns({
    port: 4711,
    addresses: ['192.0.2.7'],
    log: (m) => {
      logs.push(m);
    },
  });
  mdns.start();
  t.after(() => mdns.stop());

  // The opening announcements double as the loopback probe: if they never come
  // back to us, multicast reception is unavailable here and the goodbye could not
  // be observed either.
  const announcement = await inbox.waitFor(
    (p) => p.isResponse && p.answers.some((r) => (r.ttl ?? 0) > 0),
    'an announcement',
  );
  if (!announcement) {
    t.skip(
      `multicast loopback does not deliver to this host — cannot observe goodbyes. logs: ${JSON.stringify(logs)}`,
    );
    return;
  }
  assert.ok(
    announcement.answers.some((r) => r.typeName === 'A' && r.data === '192.0.2.7'),
    'the announcement should carry our A record',
  );

  inbox.packets.length = 0;
  await mdns.stop();

  const goodbye = await inbox.waitFor(
    (p) => p.isResponse && p.answers.length > 0 && p.answers.every((r) => r.ttl === 0),
    'the goodbye',
  );
  assert.ok(
    goodbye,
    `stop() must emit a goodbye. packets seen: ${JSON.stringify(inbox.packets.map((p) => p.answers.map((r) => `${String(r.typeName)}/${String(r.ttl)}`)))}`,
  );
  assert.ok(
    goodbye.answers.every((r) => r.ttl === 0),
    'every withdrawn record is TTL 0',
  );
  assert.ok(
    goodbye.answers.some((r) => r.typeName === 'A' && r.data === '192.0.2.7'),
    'the A record must be withdrawn',
  );
  assert.ok(
    goodbye.answers.some((r) => r.typeName === 'PTR' && r.name === SVC),
    'the service PTR must be withdrawn',
  );
});

test('update() retires the old A record (TTL 0) and announces the new one (BUG-118)', async (t) => {
  // Same multicast-loopback requirement as the goodbye test above: an
  // interface-change refresh has no question to answer, so both packets only
  // ever reach 224.0.0.251.
  const listener = await bindShared(MDNS_PORT);
  if (!listener) {
    t.skip('udp4 port 5353 is already owned by another responder');
    return;
  }
  t.after(() => close(listener));
  try {
    listener.addMembership(MDNS_ADDR);
  } catch (err) {
    t.skip(`cannot join ${MDNS_ADDR} in this environment (${errText(err)})`);
    return;
  }
  const inbox = collect(listener);

  const logs: string[] = [];
  const mdns = createMdns({
    port: 4711,
    addresses: ['192.0.2.7'],
    log: (m) => {
      logs.push(m);
    },
  });
  mdns.start();
  t.after(() => mdns.stop());

  // The startup announcement doubles as the loopback probe (see the goodbye test).
  const opening = await inbox.waitFor(
    (p) => p.isResponse && p.answers.some((r) => (r.ttl ?? 0) > 0),
    'the startup announcement',
  );
  if (!opening) {
    t.skip(
      `multicast loopback does not deliver to this host — cannot observe the refresh. logs: ${JSON.stringify(logs)}`,
    );
    return;
  }
  inbox.packets.length = 0;

  // The host roams from 192.0.2.7 (network A) to 192.0.2.9 (network B).
  assert.equal(mdns.update(['192.0.2.9']), true, 'an address change must be applied');
  assert.equal(mdns.update(['192.0.2.9']), false, 're-applying the same set is a no-op');

  // RFC 6762 §10.1: the withdrawn address goes out TTL 0, as exactly the A
  // record being retired — the shared service records (PTR/SRV/TXT) are NOT
  // touched, so a roam must not flush a healthy service cache.
  const goodbye = await inbox.waitFor(
    (p) => p.isResponse && p.answers.length > 0 && p.answers.every((r) => r.ttl === 0),
    'the TTL-0 goodbye for the removed address',
  );
  assert.ok(
    goodbye,
    `update() must say goodbye for the removed address. packets: ${JSON.stringify(inbox.packets.map((p) => p.answers.map((r) => `${String(r.typeName)}/${String(r.ttl)}/${JSON.stringify(r.data)}`)))}`,
  );
  assert.deepEqual(
    goodbye.answers,
    [
      {
        name: HOST,
        type: TYPE.A,
        typeName: 'A',
        class: 1,
        flush: false,
        ttl: 0,
        data: '192.0.2.7',
      },
    ],
    'the goodbye is exactly the retired A record',
  );

  const announced = await inbox.waitFor(
    (p) =>
      p.isResponse &&
      p.answers.some((r) => r.typeName === 'A' && r.data === '192.0.2.9' && (r.ttl ?? 0) > 0),
    'an announcement carrying the new address',
  );
  assert.ok(announced, 'the new address must be announced on the group');
  assert.ok(
    !announced.answers.some((r) => r.typeName === 'A' && r.data === '192.0.2.7'),
    'the retired address must not be re-announced',
  );

  // An empty update (every interface momentarily gone mid-roam) must not empty
  // the advertisement — start()'s "never advertise a dead host" rule holds here too.
  assert.equal(mdns.update([]), false, 'an empty address set never empties the advertisement');
  assert.equal(
    mdns.update(['not-an-ip']),
    false,
    'junk input never empties the advertisement either',
  );

  // A legacy unicast query after the roam is answered from the NEW set.
  const asker = await bindShared(0);
  if (!asker) {
    t.skip('no ephemeral udp4 socket for the legacy query');
    return;
  }
  t.after(() => close(asker));
  const queryInbox = collect(asker);
  const query = encodeMessage({
    id: 0x1234,
    flags: 0,
    questions: [{ name: HOST, type: TYPE.A, class: 1 }],
  });
  asker.send(query, MDNS_PORT, '127.0.0.1');
  const reply = await queryInbox.waitFor(
    (p) => p.isResponse && p.answers.some((r) => r.typeName === 'A'),
    'the post-update A answer',
  );
  assert.ok(reply, 'the responder must answer queries after an update');
  assert.deepEqual(
    reply.answers.filter((r) => r.typeName === 'A').map((r) => r.data),
    ['192.0.2.9'],
    'post-roam answers must carry the new address, not the retired one',
  );
});

// ------------------------------------------------- probing & conflict handling
//
// BUG-133: our A/SRV records carry the cache-flush bit — a claim of UNIQUE
// ownership — but startup announced them without the RFC 6762 §8.1 probe, and
// responses that could signal a conflict were discarded. Two hosts on the
// default `fleetdeck.local` then both answered authoritatively with different
// addresses. These tests pin the probe-first lifecycle and the stand-down.

test('buildProbeQuestions asks QU questions for every unique name, carrying our records as tie-break authorities', () => {
  const { questions, authorities } = buildProbeQuestions(AD);

  // RFC 6762 §8.1: the QU bit is set so answers come back unicast, and the
  // questions name exactly the records we intend to claim — the host A and one
  // SRV per advertised service type.
  assert.deepEqual(
    questions.map((q) => [q.name, q.type, q.unicast]),
    [
      [HOST, TYPE.A, true],
      [`Fleet Deck.${SVC}`, TYPE.SRV, true],
      [`Fleet Deck.${HTTP_SVC}`, TYPE.SRV, true],
    ],
  );

  const authorityNames = authorities.map((r) => `${r.typeName ?? r.type} ${r.name}`);
  assert.ok(
    authorityNames.includes(`A ${HOST}`),
    'our A record is the tie-break authority for the host question',
  );
  assert.ok(authorityNames.includes(`SRV Fleet Deck.${SVC}`));
  assert.ok(authorityNames.includes(`SRV Fleet Deck.${HTTP_SVC}`));
  assert.ok(
    authorities.every((r) => r.ttl === 0),
    'probe authorities carry TTL 0 — we are asking, not publishing',
  );
});

test('uniqueConflict flags a probe answer for our host with a different address', () => {
  const theirAnswer = encodeMessage({
    answers: [{ name: HOST, type: 'A', ttl: 120, flush: true, data: '192.0.2.99' }],
  });
  assert.equal(
    uniqueConflict(theirAnswer, AD, { phase: 'probing' }),
    true,
    'someone else answering our unique name during probing means we lost the claim',
  );
});

test("uniqueConflict does not flag our own looped-back records or a stranger's names", () => {
  // Multicast loopback echoes our own probe: identical authorities must not be
  // self-detected as a conflict, or no host could ever finish probing.
  const { questions, authorities } = buildProbeQuestions(AD);
  const ownProbe = encodeMessage({ id: 0, flags: 0, questions, authorities });
  assert.equal(
    uniqueConflict(ownProbe, AD, { phase: 'probing' }),
    false,
    'our own probe echo is not a conflict',
  );

  const ownAnnouncement = encodeMessage({ answers: buildAnnouncement(AD) });
  assert.equal(
    uniqueConflict(ownAnnouncement, AD, { phase: 'announced' }),
    false,
    'our own announcement echo is not a conflict',
  );

  const strangers = encodeMessage({
    answers: [{ name: 'someone-else.local', type: 'A', ttl: 120, flush: true, data: '192.0.2.99' }],
  });
  for (const phase of ['probing', 'announced'] as const) {
    assert.equal(
      uniqueConflict(strangers, AD, { phase }),
      false,
      `a neighbour's records are none of our business (${phase})`,
    );
  }
});

test('uniqueConflict applies the lexicographic tie-break between simultaneous probes', () => {
  // Two hosts probe the same name at the same time, each carrying its would-be
  // records as authorities: the lexicographically LATER authority data wins and
  // probes again; the earlier one yields (RFC 6762 §8.1). The competing probes
  // below mirror ours record-for-record so ONLY the rdata decides.
  const rivalAuthorities = (data: string): DnsRecord[] => [
    { name: HOST, type: 'A', ttl: 0, flush: false, data },
    {
      name: `Fleet Deck.${SVC}`,
      type: 'SRV',
      ttl: 0,
      flush: false,
      data: { priority: 0, weight: 0, port: 4711, target: HOST },
    },
    {
      name: `Fleet Deck.${HTTP_SVC}`,
      type: 'SRV',
      ttl: 0,
      flush: false,
      data: { priority: 0, weight: 0, port: 4711, target: HOST },
    },
  ];
  const rivalProbe = (data: string): Buffer =>
    encodeMessage({
      flags: 0,
      questions: [{ name: HOST, type: TYPE.A, class: 1 }],
      authorities: rivalAuthorities(data),
    });

  assert.equal(
    uniqueConflict(rivalProbe('255.255.255.255'), AD, { phase: 'probing' }),
    true,
    'a later-sorting probe authority beats ours',
  );
  assert.equal(
    uniqueConflict(rivalProbe('0.0.0.1'), AD, { phase: 'probing' }),
    false,
    'an earlier-sorting probe authority loses to ours',
  );
  assert.equal(
    uniqueConflict(rivalProbe('192.0.2.7'), AD, { phase: 'probing' }),
    false,
    'an identical probe (our own loopback, or a twin with the same data) is a tie, not a conflict',
  );

  const emptyProbe = encodeMessage({
    flags: 0,
    questions: [{ name: HOST, type: TYPE.A, class: 1 }],
  });
  assert.equal(
    uniqueConflict(emptyProbe, AD, { phase: 'probing' }),
    false,
    'a probe with no authority data concedes',
  );
});

test('uniqueConflict flags a clashing record after the name was claimed, in any phase', () => {
  // The conflict window never closes: once announced, a response carrying an
  // SRV that points somewhere we do not is someone else on our name (§9).
  const clashingSrv = encodeMessage({
    answers: [
      {
        name: INSTANCE,
        type: 'SRV',
        ttl: 120,
        flush: true,
        data: { priority: 0, weight: 0, port: 9999, target: HOST },
      },
    ],
  });
  assert.equal(uniqueConflict(clashingSrv, AD, { phase: 'announced' }), true);
  assert.equal(
    uniqueConflict(clashingSrv, AD, { phase: 'probing' }),
    true,
    'an answer in the Answer section conflicts during probing too',
  );
});

/** The socket tests below need udp4 port 5353 and a multicast join. Both can be
 * unavailable for legitimate reasons (a real avahi already owns the port; a
 * container with no multicast), so they SKIP with a reason rather than fail. */

test('start() probes before it announces, and a conflicting answer during probing disables discovery', async (t) => {
  const listener = await bindShared(MDNS_PORT);
  if (!listener) {
    t.skip('udp4 port 5353 is already owned by another responder');
    return;
  }
  t.after(() => close(listener));
  try {
    listener.addMembership(MDNS_ADDR);
  } catch (err) {
    t.skip(`cannot join ${MDNS_ADDR} in this environment (${errText(err)})`);
    return;
  }
  const inbox = collect(listener);

  const logs: string[] = [];
  const mdns = createMdns({
    port: 4711,
    addresses: ['192.0.2.7'],
    log: (m) => {
      logs.push(m);
    },
  });
  mdns.start();
  t.after(() => mdns.stop());

  // RFC 6762 §8.1: the FIRST thing on the wire must be probe queries for our
  // unique names — never the announcement. The looped-back probes double as the
  // multicast-reception probe; without them nothing below can be observed here.
  const probe = await inbox.waitFor(
    (p) => !p.isResponse && p.questions.some((q) => q.name === HOST),
    'a probe query for our host name',
  );
  if (!probe) {
    t.skip(
      `multicast loopback does not deliver to this host — cannot observe probes. logs: ${JSON.stringify(logs)}`,
    );
    return;
  }
  assert.equal(
    probe.authorities.length,
    3,
    'the probe carries our unique records as tie-break authorities',
  );
  assert.ok(
    probe.questions.every((q) => q.unicast),
    'probe questions ask for unicast answers (§8.1)',
  );
  assert.ok(
    !inbox.packets.some((p) => p.isResponse && p.answers.some((r) => (r.ttl ?? 0) > 0)),
    'no announcement may go out before probing has finished',
  );

  // Another device already owns the name: answer the probe with a DIFFERENT
  // address. The responder must stand down, not announce alongside.
  listener.send(
    encodeMessage({
      answers: [{ name: HOST, type: 'A', ttl: 120, flush: true, data: '192.0.2.99' }],
    }),
    MDNS_PORT,
    MDNS_ADDR,
  );

  const deadline = Date.now() + scaleMs(4000);
  while (
    !logs.some((m) => m.includes('mdns disabled') && m.includes('conflict')) &&
    Date.now() < deadline
  ) {
    await delay(25);
  }
  assert.ok(
    logs.some((m) => m.includes('mdns disabled') && m.includes('conflict')),
    `a probe answer claiming our name must disable discovery. logs: ${JSON.stringify(logs)}`,
  );
  assert.ok(
    !inbox.packets.some(
      (p) => p.isResponse && p.answers.some((r) => (r.ttl ?? 0) > 0 && r.data === '192.0.2.7'),
    ),
    'we must never publish our records onto a name someone else owns',
  );
});

test('start() announces only after a quiet probing window, and a later conflict withdraws our records', async (t) => {
  const listener = await bindShared(MDNS_PORT);
  if (!listener) {
    t.skip('udp4 port 5353 is already owned by another responder');
    return;
  }
  t.after(() => close(listener));
  try {
    listener.addMembership(MDNS_ADDR);
  } catch (err) {
    t.skip(`cannot join ${MDNS_ADDR} in this environment (${errText(err)})`);
    return;
  }
  const inbox = collect(listener);

  const logs: string[] = [];
  const mdns = createMdns({
    port: 4711,
    addresses: ['192.0.2.7'],
    log: (m) => {
      logs.push(m);
    },
  });
  mdns.start();
  t.after(() => mdns.stop());

  // Probe first, THEN the announcement carrying our A record. If neither is
  // observable, multicast reception is unavailable here and there is nothing
  // to test in this environment.
  const probeSeen = await inbox.waitFor(
    (p) => !p.isResponse && p.questions.some((q) => q.name === HOST),
    'a probe',
  );
  if (!probeSeen) {
    t.skip('multicast loopback does not deliver to this host — cannot observe probing');
    return;
  }
  const announcement = await inbox.waitFor(
    (p) =>
      p.isResponse &&
      p.answers.some((r) => (r.ttl ?? 0) > 0 && r.typeName === 'A' && r.data === '192.0.2.7'),
    'the announcement after probing',
  );
  assert.ok(
    announcement,
    `probing finished quietly but no announcement followed. logs: ${JSON.stringify(logs)}`,
  );
  assert.ok(
    logs.some((m) => m.includes('mdns responding')),
    'the claimed name is reported once probing wins',
  );

  // Post-claim conflict (RFC 6762 §9): a response carrying OUR name but THEIR
  // address. Defending forever is a war, so we withdraw (TTL 0) and stand down.
  listener.send(
    encodeMessage({
      answers: [{ name: HOST, type: 'A', ttl: 120, flush: true, data: '192.0.2.99' }],
    }),
    MDNS_PORT,
    MDNS_ADDR,
  );

  const goodbye = await inbox.waitFor(
    (p) =>
      p.isResponse &&
      p.answers.length > 0 &&
      p.answers.some((r) => r.ttl === 0 && r.typeName === 'A' && r.data === '192.0.2.7'),
    'the goodbye withdrawing our A record',
  );
  assert.ok(
    goodbye,
    `a post-claim conflict must withdraw our records. packets: ${JSON.stringify(inbox.packets.map((p) => p.answers.map((r) => `${String(r.typeName)}/${String(r.ttl)}`)))}`,
  );
  const deadline = Date.now() + scaleMs(4000);
  while (
    !logs.some((m) => m.includes('mdns disabled') && m.includes('conflict')) &&
    Date.now() < deadline
  ) {
    await delay(25);
  }
  assert.ok(
    logs.some((m) => m.includes('mdns disabled') && m.includes('conflict')),
    `a post-claim conflict must disable discovery. logs: ${JSON.stringify(logs)}`,
  );
});

// ------------------------------------------------------- degrading safely

test('createMdns never throws and start()/stop() are idempotent, whatever it is handed', async () => {
  const logs: string[] = [];

  // No port, no addresses, junk addresses: each must degrade to a quiet no-op.
  for (const options of [
    {},
    { port: 4711 },
    { port: 4711, addresses: ['not-an-ip'] },
    { port: 0, addresses: ['192.0.2.7'] },
  ] as {
    port?: number;
    addresses?: string[];
  }[]) {
    const mdns = createMdns({
      ...options,
      log: (m) => {
        logs.push(m);
      },
    });
    assert.doesNotThrow(() => {
      mdns.start();
      mdns.start();
    });
    await assert.doesNotReject(async () => {
      await mdns.stop();
      await mdns.stop();
    });
  }

  assert.ok(
    logs.every((m) => m.includes('mdns disabled')),
    'a degraded responder says why, exactly once each',
  );
  assert.equal(logs.length, 4, 'one line per dead responder — no repeats');

  // A broken logger must not be able to take the daemon down either.
  const hostile = createMdns({
    port: 4711,
    log: () => {
      throw new Error('logger exploded');
    },
  });
  assert.doesNotThrow(() => {
    hostile.start();
  });
  await assert.doesNotReject(async () => {
    await hostile.stop();
  });
});

test('stop() before start() is a no-op that resolves', async () => {
  const mdns = createMdns({
    port: 4711,
    addresses: ['192.0.2.7'],
    log: () => {
      /* silent */
    },
  });
  await assert.doesNotReject(async () => {
    await mdns.stop();
  });
  // start() after stop() must not resurrect a stopped responder.
  assert.doesNotThrow(() => {
    mdns.start();
  });
  await mdns.stop();
});

// ------------------------------------------- terminal-disable notification
//
// BUG-122: start() returns before bind + membership resolve, so the daemon
// published the .local URL even when the responder disabled itself one tick
// later. onDown is the publisher's signal to retract the URL; alive() is what
// /state consults so the share panel stops offering it.

test('a terminally degraded responder fires onDown with the reason and reports alive() false', () => {
  for (const options of [
    {},
    { port: 4711 },
    { port: 4711, addresses: ['not-an-ip'] },
    { port: 0, addresses: ['192.0.2.7'] },
  ] as {
    port?: number;
    addresses?: string[];
  }[]) {
    const downs: string[] = [];
    const mdns = createMdns({
      ...options,
      log: () => {
        /* silent */
      },
      onDown: (reason) => {
        downs.push(reason);
      },
    });
    mdns.start();
    assert.equal(
      mdns.alive(),
      false,
      `a degraded responder is not alive: ${JSON.stringify(options)}`,
    );
    assert.equal(
      downs.length,
      1,
      `onDown fires exactly once per dead responder: ${JSON.stringify(options)}`,
    );
    assert.ok((downs[0] ?? '').length > 0, 'onDown carries the disable reason');
    mdns.start(); // idempotent: a second start must not re-fire onDown
    assert.equal(downs.length, 1);
  }
});

test('stop() never fires onDown — a clean goodbye is not a failure', async (t) => {
  // Deterministic half: a responder that degraded BEFORE any socket exists.
  const early: string[] = [];
  const degraded = createMdns({
    log: () => {
      /* silent */
    },
    onDown: (reason) => {
      early.push(reason);
    },
  });
  degraded.start();
  assert.equal(early.length, 1, 'the boot-time disable fired once');
  await degraded.stop();
  assert.equal(early.length, 1, 'stop() after a disable does not re-fire onDown');

  // Live half: a responder that actually bound must report alive and stay
  // silent through a clean stop. Real sockets are environment-shaped — skip
  // rather than fail when 5353/multicast are unavailable, exactly like the
  // wire tests above.
  const logs: string[] = [];
  const downs: string[] = [];
  const mdns = createMdns({
    port: 4711,
    addresses: ['192.0.2.7'],
    log: (m) => {
      logs.push(m);
    },
    onDown: (reason) => {
      downs.push(reason);
    },
  });
  mdns.start();
  t.after(() => mdns.stop());
  await delay(scaleMs(250));
  const disabled = logs.find((m) => m.includes('mdns disabled'));
  if (disabled !== undefined) {
    t.skip(`responder degraded to a no-op in this environment: ${disabled}`);
    return;
  }

  assert.equal(mdns.alive(), true, 'a bound responder reports alive');
  assert.equal(downs.length, 0, 'a healthy responder never fires onDown');
  await mdns.stop();
  assert.equal(downs.length, 0, 'stop() is a shutdown, not a disable');
  assert.equal(mdns.alive(), false, 'a stopped responder is not alive');
});

test('a throwing onDown listener cannot take the responder down', () => {
  const mdns = createMdns({
    log: () => {
      /* silent */
    },
    onDown: () => {
      throw new Error('listener exploded');
    },
  });
  assert.doesNotThrow(() => {
    mdns.start();
  });
  assert.equal(mdns.alive(), false);
});

test('update() revives a responder that started with no LAN address at all', async (t) => {
  const probe = await bindShared(MDNS_PORT);
  if (!probe) {
    t.skip('udp4 port 5353 is already owned by another responder');
    return;
  }
  await close(probe);
  if (await foreignResponderOn5353()) {
    t.skip('another responder shares udp/5353 — unicast delivery is ambiguous in this environment');
    return;
  }

  const logs: string[] = [];
  // Boot with the network still down: no address, nothing to advertise.
  const mdns = createMdns({
    port: 4711,
    addresses: [],
    log: (m) => {
      logs.push(m);
    },
  });
  mdns.start();
  t.after(() => mdns.stop());
  assert.ok(
    logs.some((m) => m.includes('no non-internal IPv4 address')),
    'the dormant reason must be logged',
  );

  // The network comes up: the responder must bind, join and answer — not stay
  // dead for the daemon's lifetime (the old one-way door, BUG-129).
  mdns.update({ addresses: ['192.0.2.7'] });
  await delay(scaleMs(250));
  const disabled = logs.find(
    (m) => m.includes('mdns disabled') && !m.includes('no non-internal IPv4'),
  );
  if (disabled !== undefined) {
    t.skip(`responder degraded to a no-op in this environment: ${disabled}`);
    return;
  }

  const asker = await bindShared(0);
  assert.ok(asker, 'the test needs an ephemeral udp4 socket');
  t.after(() => close(asker));
  const inbox = collect(asker);
  asker.send(
    encodeMessage({ id: 3, flags: 0, questions: [{ name: HOST, type: TYPE.A, class: 1 }] }),
    MDNS_PORT,
    '127.0.0.1',
  );

  const reply = await inbox.waitFor(
    (p) => p.isResponse && p.answers.some((r) => r.typeName === 'A'),
    'the A answer after revival',
  );
  assert.ok(
    reply,
    `a responder revived by update() must answer queries. logs: ${JSON.stringify(logs)}`,
  );
  const revivedA = reply.answers.find((r) => r.typeName === 'A');
  assert.ok(revivedA);
  assert.equal(revivedA.data, '192.0.2.7');
});
