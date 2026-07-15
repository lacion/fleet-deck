// tests/mdns.test.mjs
//
// The mDNS/DNS-SD responder (scripts/fleetd/mdns.mjs) is two things stacked:
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

import test from 'node:test';
import assert from 'node:assert/strict';
import dgram from 'node:dgram';
import {
  createMdns,
  buildAnnouncement,
  buildResponse,
  decodeMessage,
  decodeName,
  encodeMessage,
  encodeName,
  encodeRecord,
  normalize,
  parseQuestions,
  META_QUERY,
  MDNS_ADDR,
  MDNS_PORT,
  TYPE,
} from '../scripts/fleetd/mdns.mjs';
import { scaleMs } from './helpers/wait.mjs';

const AD = { port: 4711, addresses: ['192.0.2.7'] }; // RFC 5737 TEST-NET-1: never routable
const HOST = 'fleetdeck.local';
const SVC = '_fleetdeck._tcp.local';
const HTTP_SVC = '_http._tcp.local';
const INSTANCE = 'Fleet Deck._fleetdeck._tcp.local';

const only = (records, type) => records.filter(r => r.type === type || r.typeName === type);

// ------------------------------------------------------------- the codec

test('encodeName/decodeName round-trip a dotted name', () => {
  const buf = encodeName(HOST);
  // length-prefixed labels, root NUL: 9 'fleetdeck' 5 'local' 0
  assert.deepEqual(buf, Buffer.from([9, ...Buffer.from('fleetdeck'), 5, ...Buffer.from('local'), 0]));
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
  const base = encodeName(HOST);                       // [0..16]
  const localAt = base.indexOf(5);                     // offset of the "local" label
  const tail = Buffer.concat([
    Buffer.from([5, ...Buffer.from('_http'), 4, ...Buffer.from('_tcp')]),
    Buffer.from([0xc0 | ((localAt >> 8) & 0x3f), localAt & 0xff]),
  ]);
  const packet = Buffer.concat([base, tail]);

  const decoded = decodeName(packet, base.length);
  assert.equal(decoded.name, '_http._tcp.local', 'the pointer must expand to the shared suffix');
  assert.equal(decoded.offset, packet.length, 'offset lands after the 2 pointer bytes, not after its target');
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
  header.writeUInt16BE(2, 4);      // QDCOUNT

  const q1name = encodeName(HOST);
  const q1 = Buffer.alloc(4);
  q1.writeUInt16BE(TYPE.A, 0);
  q1.writeUInt16BE(0x8001, 2);     // QU bit + class IN

  const localAt = 12 + q1name.indexOf(5);
  const q2name = Buffer.concat([
    Buffer.from([10, ...Buffer.from('_fleetdeck'), 4, ...Buffer.from('_tcp')]),
    Buffer.from([0xc0 | ((localAt >> 8) & 0x3f), localAt & 0xff]),
  ]);
  const q2 = Buffer.alloc(4);
  q2.writeUInt16BE(TYPE.PTR, 0);
  q2.writeUInt16BE(0x0001, 2);     // no QU bit

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
  const a = decodeMessage(encodeMessage({ answers: [{ name: HOST, type: 'A', ttl: 120, flush: true, data: '192.0.2.7' }] }));
  assert.equal(a.answers[0].data, '192.0.2.7');
  assert.equal(a.answers[0].flush, true);
  assert.equal(a.answers[0].class, 1, 'the flush bit must not leak into the class');

  const srv = decodeMessage(encodeMessage({
    answers: [{ name: INSTANCE, type: 'SRV', ttl: 120, flush: true, data: { priority: 0, weight: 0, port: 4711, target: HOST } }],
  }));
  assert.deepEqual(srv.answers[0].data, { priority: 0, weight: 0, port: 4711, target: HOST });

  const txt = decodeMessage(encodeMessage({
    answers: [{ name: INSTANCE, type: 'TXT', ttl: 4500, flush: false, data: { path: '/', board: 'fleetdeck' } }],
  }));
  assert.deepEqual(txt.answers[0].data, ['path=/', 'board=fleetdeck']);
  assert.equal(txt.answers[0].flush, false);

  const ptr = decodeMessage(encodeMessage({ answers: [{ name: SVC, type: 'PTR', ttl: 4500, flush: false, data: INSTANCE }] }));
  assert.equal(ptr.answers[0].data, INSTANCE);

  // A record we do not own must not be silently encoded as garbage.
  assert.throws(() => encodeRecord({ name: HOST, type: 'AAAA', ttl: 120, data: '::1' }), /rdata/);
});

// -------------------------------------------------------- what we answer

test('buildResponse answers an A query for fleetdeck.local with every advertised address', () => {
  const { answers, additionals } = buildResponse([{ name: HOST, type: TYPE.A, class: 1 }], {
    port: 4711, addresses: ['192.0.2.7', '192.0.2.8'],
  });

  assert.equal(answers.length, 2);
  assert.deepEqual(answers.map(r => r.data), ['192.0.2.7', '192.0.2.8']);
  for (const r of answers) {
    assert.equal(r.name, HOST);
    assert.equal(r.type, 'A');
    assert.equal(r.ttl, 120, 'RFC 6762 §10: a host record gets the 120s TTL');
    assert.equal(r.flush, true, 'we uniquely own our own A record');
  }
  assert.equal(additionals.length, 0, 'an A query needs no extras');
});

test('buildResponse matches names case-insensitively and answers ANY', () => {
  const upper = buildResponse([{ name: 'FleetDeck.LOCAL', type: TYPE.A, class: 1 }], AD);
  assert.equal(upper.answers.length, 1, 'RFC 6762 §16: names compare case-insensitively');

  const any = buildResponse([{ name: HOST, type: TYPE.ANY, class: 1 }], AD);
  assert.equal(any.answers.length, 1);
  assert.equal(any.answers[0].type, 'A');
});

test('buildResponse stays silent for AAAA, for a stranger\'s name, and for a service that is not ours', () => {
  for (const q of [
    { name: HOST, type: TYPE.AAAA, class: 1 },
    { name: 'someone-else.local', type: TYPE.A, class: 1 },
    { name: '_ssh._tcp.local', type: TYPE.PTR, class: 1 },
    { name: HOST, type: TYPE.A, class: 3 }, // not class IN
  ]) {
    const { answers } = buildResponse([q], AD);
    assert.equal(answers.length, 0, `must not answer ${q.name}/${q.type}/class ${q.class}`);
  }
});

test('buildResponse answers a PTR browse with SRV + TXT + A in the ADDITIONAL section', () => {
  // THE one that matters. RFC 6763 §12.1: without these extras a browser shows a
  // name it cannot connect to, and needs two more round-trips to fix that.
  const { answers, additionals } = buildResponse([{ name: SVC, type: TYPE.PTR, class: 1 }], AD);

  assert.equal(answers.length, 1);
  assert.equal(answers[0].name, SVC);
  assert.equal(answers[0].type, 'PTR');
  assert.equal(answers[0].data, INSTANCE);
  assert.equal(answers[0].ttl, 4500);
  assert.equal(answers[0].flush, false, 'PTR is a SHARED record set — flushing it would evict our neighbours');

  const srv = only(additionals, 'SRV');
  const txt = only(additionals, 'TXT');
  const a = only(additionals, 'A');
  assert.equal(srv.length, 1, 'SRV must ride along');
  assert.equal(txt.length, 1, 'TXT must ride along');
  assert.equal(a.length, 1, 'the A record for the SRV target must ride along');

  assert.equal(srv[0].name, INSTANCE);
  assert.deepEqual(srv[0].data, { priority: 0, weight: 0, port: 4711, target: HOST });
  assert.equal(txt[0].data.path, '/');
  assert.equal(txt[0].data.board, 'fleetdeck');
  assert.equal(a[0].name, HOST);
  assert.equal(a[0].data, '192.0.2.7');
});

test('buildResponse answers a PTR browse for the generic _http._tcp type too', () => {
  const { answers, additionals } = buildResponse([{ name: HTTP_SVC, type: TYPE.PTR, class: 1 }], AD);
  assert.equal(answers.length, 1);
  assert.equal(answers[0].data, `Fleet Deck.${HTTP_SVC}`);
  assert.deepEqual(only(additionals, 'SRV')[0].data.port, 4711);
  assert.equal(only(additionals, 'A').length, 1);
});

test('buildResponse answers the _services._dns-sd._udp meta-query with both service types', () => {
  const { answers } = buildResponse([{ name: META_QUERY, type: TYPE.PTR, class: 1 }], AD);
  assert.deepEqual(answers.map(r => r.data).sort(), [HTTP_SVC, SVC].sort());
  for (const r of answers) {
    assert.equal(r.name, META_QUERY);
    assert.equal(r.type, 'PTR');
    assert.equal(r.flush, false);
  }
});

test('buildResponse resolves the instance name: SRV carries the A record, TXT stands alone', () => {
  const srv = buildResponse([{ name: INSTANCE, type: TYPE.SRV, class: 1 }], AD);
  assert.equal(srv.answers.length, 1);
  assert.equal(srv.answers[0].type, 'SRV');
  assert.equal(only(srv.additionals, 'A').length, 1, 'RFC 6763 §12.2: the SRV target needs its address');

  const txt = buildResponse([{ name: INSTANCE, type: TYPE.TXT, class: 1 }], AD);
  assert.equal(txt.answers.length, 1);
  assert.equal(txt.answers[0].type, 'TXT');
});

test('buildResponse de-duplicates across questions and never repeats an answer as an additional', () => {
  const { answers, additionals } = buildResponse([
    { name: SVC, type: TYPE.PTR, class: 1 },
    { name: HOST, type: TYPE.A, class: 1 },   // the A is also a PTR additional
    { name: SVC, type: TYPE.PTR, class: 1 },  // asked twice
  ], AD);

  assert.equal(only(answers, 'PTR').length, 1, 'the PTR is answered once');
  assert.equal(only(answers, 'A').length, 1);
  assert.equal(only(additionals, 'A').length, 0, 'an A already in ANSWER must not be repeated in ADDITIONAL');
});

test('buildResponse with no advertisable address answers nothing at all', () => {
  const { answers } = buildResponse([{ name: HOST, type: TYPE.A, class: 1 }], { port: 4711, addresses: [] });
  assert.equal(answers.length, 0, 'better silent than pointing at a host with no address');
});

test('normalize drops junk addresses and turns a dotted instance into one legal label', () => {
  const ad = normalize({ port: 4711, addresses: ['192.0.2.7', '::1', '999.1.1.1', 'nonsense'], instance: 'Luis. Deck' });
  assert.deepEqual(ad.addresses, ['192.0.2.7'], 'only real IPv4 goes in an A record');
  assert.ok(!ad.instance.includes('.'), 'a dot in an instance name would split it into two labels');
  assert.equal(ad.host, HOST);
});

// ------------------------------------------------- announcements & goodbyes

test('buildAnnouncement carries the whole advertisement in one Answer section', () => {
  const records = buildAnnouncement(AD);
  const names = records.map(r => `${r.typeName || r.type} ${r.name}`);

  assert.deepEqual(new Set(names), new Set([
    `A ${HOST}`,
    `PTR ${META_QUERY}`, // once per service type — the Set collapses them
    `PTR ${SVC}`, `SRV Fleet Deck.${SVC}`, `TXT Fleet Deck.${SVC}`,
    `PTR ${HTTP_SVC}`, `SRV Fleet Deck.${HTTP_SVC}`, `TXT Fleet Deck.${HTTP_SVC}`,
  ]));
  assert.equal(only(records, 'PTR').filter(r => r.name === META_QUERY).length, 2, 'both types listed under the meta-query');
  assert.ok(records.every(r => r.ttl > 0), 'a live announcement never has a zero TTL');
});

test('a goodbye is the same record set with TTL 0 and no cache-flush claim', () => {
  const alive = buildAnnouncement(AD);
  const goodbye = buildAnnouncement(AD, { ttl: 0 });

  assert.equal(goodbye.length, alive.length, 'we withdraw exactly what we claimed');
  assert.ok(goodbye.every(r => r.ttl === 0), 'RFC 6762 §10.1: a goodbye is TTL 0');
  assert.ok(goodbye.every(r => r.flush === false), 'a record being withdrawn must not also claim uniqueness');
});

// ------------------------------------------------------------ the socket

/** Bind a udp4 socket, resolving null (rather than throwing) if 5353 is taken. */
function bindShared(port) {
  return new Promise((resolve) => {
    const socket = dgram.createSocket({ type: 'udp4', reuseAddr: true });
    socket.once('error', () => { try { socket.close(); } catch { /* already gone */ } resolve(null); });
    socket.bind({ port }, () => resolve(socket));
  });
}

function collect(socket) {
  const packets = [];
  socket.on('message', (msg, rinfo) => {
    const decoded = decodeMessage(msg);
    if (decoded) packets.push({ ...decoded, rinfo });
  });
  return {
    packets,
    async waitFor(predicate, label, timeoutMs = 4000) {
      const deadline = Date.now() + scaleMs(timeoutMs);
      for (;;) {
        const hit = packets.find(predicate);
        if (hit) return hit;
        if (Date.now() >= deadline) return null;
        await new Promise(r => setTimeout(r, 25));
      }
    },
  };
}

const close = socket => new Promise(resolve => { try { socket.close(resolve); } catch { resolve(); } });

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
async function foreignResponderOn5353() {
  const probe = await bindShared(MDNS_PORT);
  if (!probe) return true;                          // can't even share the port — treat it as foreign-owned
  const sender = await bindShared(0);
  if (!sender) { await close(probe); return true; } // no ephemeral socket to probe with — same conclusion

  let retry, deadline;
  const arrived = await new Promise((resolve) => {
    const done = (v) => { clearTimeout(retry); clearTimeout(deadline); resolve(v); };
    probe.once('message', () => done(true));        // OUR socket received it — unicast delivery works here
    const shoot = () => { try { sender.send(Buffer.from([0]), MDNS_PORT, '127.0.0.1'); } catch { /* the point is whether it lands */ } };
    shoot();                                         // probe is already bound+listening: bindShared resolves in the bind callback
    retry = setTimeout(shoot, scaleMs(200));         // re-send once, in case scheduler lag beat the first send to the listener
    deadline = setTimeout(() => done(false), scaleMs(600));
  });

  await close(sender);
  await close(probe);
  return !arrived;                                   // datagram vanished => another responder on 5353 owns it
}

test('a real A query on the wire gets a real answer carrying the advertised IPv4', async (t) => {
  // The responder needs udp4/5353. If a real avahi owns it, standing down is the
  // CORRECT behaviour, not a failure — so skip rather than fail.
  const probe = await bindShared(MDNS_PORT);
  if (!probe) return t.skip('udp4 port 5353 is already owned by another responder (avahi/Bonjour?) — nothing to test');
  await close(probe);
  // Sharing the port can succeed while a co-bound responder (macOS Bonjour) still
  // wins our unicast datagrams. If our own probe cannot reach us, standing down is
  // correct — the legacy-query reply below would land in someone else's socket.
  if (await foreignResponderOn5353()) return t.skip('another responder shares udp/5353 (mDNSResponder/Bonjour?) — unicast delivery is ambiguous in this environment');

  const logs = [];
  const mdns = createMdns({ port: 4711, addresses: ['192.0.2.7'], log: m => logs.push(String(m)) });
  mdns.start();
  t.after(() => mdns.stop());

  // start() is async under the hood (bind + join); give it a beat, then check it
  // did not degrade. No multicast in this environment is a skip, not a failure.
  await new Promise(r => setTimeout(r, scaleMs(250)));
  const disabled = logs.find(m => m.includes('mdns disabled'));
  if (disabled) return t.skip(`responder degraded to a no-op in this environment: ${disabled}`);

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

  const reply = await inbox.waitFor(p => p.isResponse && p.answers.some(r => r.typeName === 'A'), 'the A answer');
  assert.ok(reply, `no mDNS response arrived for ${HOST}. logs: ${JSON.stringify(logs)}`);

  const a = reply.answers.find(r => r.typeName === 'A');
  assert.equal(a.name, HOST);
  assert.equal(a.data, '192.0.2.7', 'the advertised LAN address must come back on the wire');

  // RFC 6762 §6.7 — a legacy (non-5353 source port) resolver matches on the echoed
  // ID and question, and must get a short TTL with no cache-flush bit.
  assert.equal(reply.id, 0xbeef, 'a legacy query`s ID must be echoed');
  assert.deepEqual(reply.questions.map(q => q.name), [HOST], 'a legacy query`s question must be echoed');
  assert.equal(a.ttl, 10);
  assert.equal(a.flush, false);
});

test('a PTR browse on the wire resolves the board in one round-trip', async (t) => {
  const probe = await bindShared(MDNS_PORT);
  if (!probe) return t.skip('udp4 port 5353 is already owned by another responder');
  await close(probe);
  if (await foreignResponderOn5353()) return t.skip('another responder shares udp/5353 (mDNSResponder/Bonjour?) — unicast delivery is ambiguous in this environment');

  const logs = [];
  const mdns = createMdns({ port: 4711, addresses: ['192.0.2.7'], log: m => logs.push(String(m)) });
  mdns.start();
  t.after(() => mdns.stop());
  await new Promise(r => setTimeout(r, scaleMs(250)));
  const disabled = logs.find(m => m.includes('mdns disabled'));
  if (disabled) return t.skip(`responder degraded to a no-op: ${disabled}`);

  const asker = await bindShared(0);
  t.after(() => close(asker));
  const inbox = collect(asker);

  asker.send(encodeMessage({ id: 1, flags: 0, questions: [{ name: SVC, type: TYPE.PTR, class: 1 }] }), MDNS_PORT, '127.0.0.1');

  const reply = await inbox.waitFor(p => p.isResponse && p.answers.some(r => r.typeName === 'PTR'), 'the PTR answer');
  assert.ok(reply, `no PTR response arrived. logs: ${JSON.stringify(logs)}`);

  assert.equal(reply.answers[0].data, INSTANCE);
  // Everything a browser needs to open a socket, without asking a second question.
  const srv = reply.additionals.find(r => r.typeName === 'SRV');
  const a = reply.additionals.find(r => r.typeName === 'A');
  const txt = reply.additionals.find(r => r.typeName === 'TXT');
  assert.ok(srv && a && txt, 'SRV + A + TXT must all be in the additional section');
  assert.equal(srv.data.port, 4711);
  assert.equal(srv.data.target, HOST);
  assert.equal(a.data, '192.0.2.7');
  assert.ok(txt.data.includes('path=/'));
});

test('stop() puts goodbye records (TTL 0) on the multicast group', async (t) => {
  // This one genuinely needs multicast loopback: a goodbye has no question to
  // answer, so it only ever goes to 224.0.0.251. Skip if the kernel/network does
  // not loop our own multicast back to us.
  const listener = await bindShared(MDNS_PORT);
  if (!listener) return t.skip('udp4 port 5353 is already owned by another responder');
  t.after(() => close(listener));

  try {
    listener.addMembership(MDNS_ADDR);
  } catch (err) {
    return t.skip(`cannot join ${MDNS_ADDR} in this environment (${err.code || err.message})`);
  }
  const inbox = collect(listener);

  const logs = [];
  const mdns = createMdns({ port: 4711, addresses: ['192.0.2.7'], log: m => logs.push(String(m)) });
  mdns.start();
  t.after(() => mdns.stop());

  // The opening announcements double as the loopback probe: if they never come
  // back to us, multicast reception is unavailable here and the goodbye could not
  // be observed either.
  const announcement = await inbox.waitFor(p => p.isResponse && p.answers.some(r => r.ttl > 0), 'an announcement');
  if (!announcement) {
    return t.skip(`multicast loopback does not deliver to this host — cannot observe goodbyes. logs: ${JSON.stringify(logs)}`);
  }
  assert.ok(announcement.answers.some(r => r.typeName === 'A' && r.data === '192.0.2.7'),
    'the announcement should carry our A record');

  inbox.packets.length = 0;
  await mdns.stop();

  const goodbye = await inbox.waitFor(p => p.isResponse && p.answers.length > 0 && p.answers.every(r => r.ttl === 0), 'the goodbye');
  assert.ok(goodbye, `stop() must emit a goodbye. packets seen: ${JSON.stringify(inbox.packets.map(p => p.answers.map(r => `${r.typeName}/${r.ttl}`)))}`);
  assert.ok(goodbye.answers.every(r => r.ttl === 0), 'every withdrawn record is TTL 0');
  assert.ok(goodbye.answers.some(r => r.typeName === 'A' && r.data === '192.0.2.7'), 'the A record must be withdrawn');
  assert.ok(goodbye.answers.some(r => r.typeName === 'PTR' && r.name === SVC), 'the service PTR must be withdrawn');
});

// ------------------------------------------------------- degrading safely

test('createMdns never throws and start()/stop() are idempotent, whatever it is handed', async () => {
  const logs = [];

  // No port, no addresses, junk addresses: each must degrade to a quiet no-op.
  for (const options of [{}, { port: 4711 }, { port: 4711, addresses: ['not-an-ip'] }, { port: 0, addresses: ['192.0.2.7'] }]) {
    const mdns = createMdns({ ...options, log: m => logs.push(String(m)) });
    assert.doesNotThrow(() => { mdns.start(); mdns.start(); });
    await assert.doesNotReject(async () => { await mdns.stop(); await mdns.stop(); });
  }

  assert.ok(logs.every(m => m.includes('mdns disabled')), 'a degraded responder says why, exactly once each');
  assert.equal(logs.length, 4, 'one line per dead responder — no repeats');

  // A broken logger must not be able to take the daemon down either.
  const hostile = createMdns({ port: 4711, log: () => { throw new Error('logger exploded'); } });
  assert.doesNotThrow(() => hostile.start());
  await assert.doesNotReject(async () => { await hostile.stop(); });
});

test('stop() before start() is a no-op that resolves', async () => {
  const mdns = createMdns({ port: 4711, addresses: ['192.0.2.7'], log: () => {} });
  await assert.doesNotReject(async () => { await mdns.stop(); });
  // start() after stop() must not resurrect a stopped responder.
  assert.doesNotThrow(() => mdns.start());
  await mdns.stop();
});
