// mdns.mjs — a dependency-free mDNS (RFC 6762) + DNS-SD (RFC 6763) responder.
//
// CONTRACT: in LAN mode the board must be reachable as `http://fleetdeck.local:<port>`
// and must show up in service browsers, WITHOUT anyone typing an IP and WITHOUT
// avahi/Bonjour being installed. So we speak the wire protocol ourselves: one
// udp4 socket on 224.0.0.251:5353, a hand-rolled DNS codec, nothing else.
//
// mDNS is a CONVENIENCE, never a dependency. Every failure mode this can hit —
// port 5353 already owned by a real avahi, EPERM on the multicast join, an
// interface with no multicast at all, a malformed packet from a stranger on the
// LAN — is logged exactly once and degrades this module to a no-op. It must never
// take the daemon with it, and it never speaks unless spoken to (after the
// opening announcements).
//
// What we advertise, for `{ port, name: 'fleetdeck', instance: 'Fleet Deck' }`:
//
//   A    fleetdeck.local                        -> each LAN IPv4 in `addresses`
//   PTR  _fleetdeck._tcp.local                  -> Fleet Deck._fleetdeck._tcp.local
//   PTR  _http._tcp.local                       -> Fleet Deck._http._tcp.local
//   SRV  Fleet Deck.<service>                   -> port + target fleetdeck.local
//   TXT  Fleet Deck.<service>                   -> path=/ board=fleetdeck
//   PTR  _services._dns-sd._udp.local           -> both service types
//
// `_http._tcp` is carried alongside our own `_fleetdeck._tcp` on purpose: generic
// browsers (Safari's Bonjour list, `dns-sd -B _http._tcp`, most phone apps) only
// enumerate well-known types, and a board nobody can find is a board that does not
// exist. The `_services._dns-sd._udp` meta-record is what makes "list every service
// on this network" browsers see us at all (RFC 6763 §9).
//
// RFC choices worth stating, because they are not obvious:
//   - Answers to a PTR query carry SRV + TXT + A in the ADDITIONAL section
//     (RFC 6763 §12). That is the difference between a browser resolving us in one
//     round-trip and a browser showing a name it cannot connect to.
//   - Cache-flush bit (top bit of the record class) is set on the records we
//     uniquely own — A, SRV, TXT — and NOT on PTR, which is a shared record set
//     that other hosts also contribute to (RFC 6762 §10.2).
//   - TTLs are the RFC 6762 §10 defaults: 120s for records naming a host (A, SRV),
//     4500s for the rest (PTR, TXT).
//   - A query whose source port is not 5353 is a LEGACY unicast query (RFC 6762
//     §6.7): we answer it unicast, echo its ID and question, use TTL 10 and clear
//     the cache-flush bit. A query with the QU bit (top bit of the qclass) set is
//     answered unicast too, but otherwise normally.
//   - Name compression is implemented for PARSING (queries from real stacks are
//     full of pointers) and deliberately NOT used when emitting. Our packets are a
//     few hundred bytes; correctness beats the savings.
//
// Everything above the socket is pure and exported (encodeName/decodeName,
// encodeRecord, parseQuestions, buildResponse, buildAnnouncement, encodeMessage,
// decodeMessage) so the wire format is unit-testable without touching a network.

import dgram from 'node:dgram';

export const MDNS_ADDR = '224.0.0.251';
export const MDNS_PORT = 5353;

export const TYPE = { A: 1, PTR: 12, TXT: 16, AAAA: 28, SRV: 33, ANY: 255 };
const TYPE_NAME = Object.fromEntries(Object.entries(TYPE).map(([k, v]) => [v, k]));

export const CLASS_IN = 1;
export const CLASS_ANY = 255;    // a QCLASS of 255 ("any class") also matches IN
export const FLUSH_BIT = 0x8000; // top bit of an ANSWER's class: cache-flush
export const QU_BIT = 0x8000;    // top bit of a QUESTION's class: unicast reply wanted
const QR_BIT = 0x8000;           // top bit of the header flags: this is a response
const FLAGS_RESPONSE = 0x8400;   // QR + AA — an mDNS responder is always authoritative

// RFC 6762 §10: 120s for records that name a host, 4500s for everything else.
const DEFAULT_TTL = { A: 120, SRV: 120, PTR: 4500, TXT: 4500 };
const LEGACY_TTL = 10; // §6.7 — a legacy resolver's cache must not outlive us for long

export const SERVICE_TYPES = ['_fleetdeck._tcp.local', '_http._tcp.local'];
export const META_QUERY = '_services._dns-sd._udp.local';

const ANNOUNCE_DELAYS_MS = [0, 1000, 2000]; // RFC 6762 §8.3: 2-3 announcements, ~1s apart

// ------------------------------------------------------------------ wire codec

/** A dotted name -> length-prefixed labels + a root NUL. */
export function encodeName(name) {
  const labels = String(name).replace(/\.$/, '').split('.').filter(Boolean);
  const parts = [];
  for (const label of labels) {
    const bytes = Buffer.from(label, 'utf8');
    // 63 is the hard DNS label limit; normalize() guarantees we never get here
    // with a longer one, so this is a contract violation, not a runtime case.
    if (bytes.length > 63) throw new RangeError(`mdns: label longer than 63 bytes: ${label}`);
    parts.push(Buffer.from([bytes.length]), bytes);
  }
  parts.push(Buffer.from([0]));
  return Buffer.concat(parts);
}

/** Read a name at `offset`, following compression pointers (RFC 1035 §4.1.4).
 * Returns the name and the offset of the first byte AFTER the name as it was
 * written here — i.e. after the 2-byte pointer, not after the target it names. */
export function decodeName(buf, offset = 0) {
  const labels = [];
  let pos = offset;
  let end = offset;
  let jumped = false;
  let hops = 0;

  for (;;) {
    if (pos >= buf.length) throw new RangeError('mdns: name runs past end of packet');
    const len = buf[pos];

    if (len === 0) {
      pos += 1;
      if (!jumped) end = pos;
      break;
    }

    if ((len & 0xc0) === 0xc0) {
      if (pos + 1 >= buf.length) throw new RangeError('mdns: truncated compression pointer');
      const target = ((len & 0x3f) << 8) | buf[pos + 1];
      if (!jumped) { end = pos + 2; jumped = true; }
      // A pointer chain is attacker-controlled; cap it rather than spin forever.
      if (++hops > 64) throw new RangeError('mdns: compression pointer loop');
      pos = target;
      continue;
    }

    if ((len & 0xc0) !== 0) throw new RangeError('mdns: reserved label type');
    const start = pos + 1;
    if (start + len > buf.length) throw new RangeError('mdns: label runs past end of packet');
    labels.push(buf.toString('utf8', start, start + len));
    pos = start + len;
    if (!jumped) end = pos;
  }

  return { name: labels.join('.'), offset: end };
}

function typeNumber(type) {
  if (typeof type === 'number') return type;
  const n = TYPE[String(type).toUpperCase()];
  if (!n) throw new TypeError(`mdns: unknown record type ${type}`);
  return n;
}

function encodeIPv4(address) {
  const octets = String(address).split('.').map(Number);
  if (octets.length !== 4 || octets.some(o => !Number.isInteger(o) || o < 0 || o > 255)) {
    throw new TypeError(`mdns: not an IPv4 address: ${address}`);
  }
  return Buffer.from(octets);
}

function encodeTxt(data) {
  const strings = Array.isArray(data)
    ? data.map(String)
    : Object.entries(data || {}).map(([k, v]) => `${k}=${v}`);
  // An empty TXT is illegal: RFC 6763 §6.1 requires a single zero-length string.
  if (!strings.length) return Buffer.from([0]);
  const parts = [];
  for (const s of strings) {
    const bytes = Buffer.from(s, 'utf8').subarray(0, 255);
    parts.push(Buffer.from([bytes.length]), bytes);
  }
  return Buffer.concat(parts);
}

function encodeRdata(type, data) {
  switch (type) {
    case TYPE.A: return encodeIPv4(data);
    case TYPE.PTR: return encodeName(data);
    case TYPE.TXT: return encodeTxt(data);
    case TYPE.SRV: {
      const head = Buffer.alloc(6);
      head.writeUInt16BE(data.priority ?? 0, 0);
      head.writeUInt16BE(data.weight ?? 0, 2);
      head.writeUInt16BE(data.port ?? 0, 4);
      return Buffer.concat([head, encodeName(data.target)]);
    }
    default: throw new TypeError(`mdns: cannot encode rdata for type ${type}`);
  }
}

/** One resource record -> wire bytes. `flush` sets the cache-flush class bit. */
export function encodeRecord(record) {
  const type = typeNumber(record.type);
  const name = encodeName(record.name);
  const rdata = encodeRdata(type, record.data);

  const head = Buffer.alloc(8);
  head.writeUInt16BE(type, 0);
  head.writeUInt16BE(CLASS_IN | (record.flush ? FLUSH_BIT : 0), 2);
  head.writeUInt32BE(Math.max(0, Number(record.ttl) || 0), 4);

  const rdlength = Buffer.alloc(2);
  rdlength.writeUInt16BE(rdata.length, 0);

  return Buffer.concat([name, head, rdlength, rdata]);
}

export function parseHeader(buf) {
  if (!Buffer.isBuffer(buf) || buf.length < 12) return null;
  return {
    id: buf.readUInt16BE(0),
    flags: buf.readUInt16BE(2),
    qdcount: buf.readUInt16BE(4),
    ancount: buf.readUInt16BE(6),
    nscount: buf.readUInt16BE(8),
    arcount: buf.readUInt16BE(10),
  };
}

/** Questions out of a query packet. Truncated/garbage tails stop the parse and
 * return what was understood — a stranger's malformed packet is not our problem. */
export function parseQuestions(buf) {
  const header = parseHeader(buf);
  if (!header) return [];

  const questions = [];
  let offset = 12;
  for (let i = 0; i < header.qdcount; i += 1) {
    let decoded;
    try { decoded = decodeName(buf, offset); } catch { break; }
    offset = decoded.offset;
    if (offset + 4 > buf.length) break;
    const type = buf.readUInt16BE(offset);
    const qclass = buf.readUInt16BE(offset + 2);
    offset += 4;
    questions.push({
      name: decoded.name,
      type,
      typeName: TYPE_NAME[type] || String(type),
      class: qclass & ~QU_BIT,
      unicast: (qclass & QU_BIT) !== 0,
    });
  }
  return questions;
}

export function encodeMessage({ id = 0, flags = FLAGS_RESPONSE, questions = [], answers = [], additionals = [] } = {}) {
  const header = Buffer.alloc(12);
  header.writeUInt16BE(id & 0xffff, 0);
  header.writeUInt16BE(flags & 0xffff, 2);
  header.writeUInt16BE(questions.length, 4);
  header.writeUInt16BE(answers.length, 6);
  header.writeUInt16BE(0, 8);
  header.writeUInt16BE(additionals.length, 10);

  const parts = [header];
  for (const q of questions) {
    const tail = Buffer.alloc(4);
    tail.writeUInt16BE(typeNumber(q.type ?? TYPE.ANY), 0);
    tail.writeUInt16BE((q.class || CLASS_IN) | (q.unicast ? QU_BIT : 0), 2);
    parts.push(encodeName(q.name), tail);
  }
  for (const record of [...answers, ...additionals]) parts.push(encodeRecord(record));
  return Buffer.concat(parts);
}

function decodeRecords(buf, offset, count) {
  const records = [];
  let pos = offset;
  for (let i = 0; i < count; i += 1) {
    let decoded;
    try { decoded = decodeName(buf, pos); } catch { break; }
    pos = decoded.offset;
    if (pos + 10 > buf.length) break;
    const type = buf.readUInt16BE(pos);
    const rclass = buf.readUInt16BE(pos + 2);
    const ttl = buf.readUInt32BE(pos + 4);
    const rdlength = buf.readUInt16BE(pos + 8);
    pos += 10;
    if (pos + rdlength > buf.length) break;

    let data;
    try {
      switch (type) {
        case TYPE.A: data = Array.from(buf.subarray(pos, pos + 4)).join('.'); break;
        case TYPE.PTR: data = decodeName(buf, pos).name; break;
        case TYPE.SRV: data = {
          priority: buf.readUInt16BE(pos),
          weight: buf.readUInt16BE(pos + 2),
          port: buf.readUInt16BE(pos + 4),
          target: decodeName(buf, pos + 6).name,
        }; break;
        case TYPE.TXT: {
          const strings = [];
          for (let p = pos; p < pos + rdlength;) {
            const len = buf[p];
            strings.push(buf.toString('utf8', p + 1, p + 1 + len));
            p += 1 + len;
          }
          data = strings;
          break;
        }
        default: data = Buffer.from(buf.subarray(pos, pos + rdlength));
      }
    } catch { data = Buffer.from(buf.subarray(pos, pos + rdlength)); }

    records.push({
      name: decoded.name,
      type,
      typeName: TYPE_NAME[type] || String(type),
      class: rclass & ~FLUSH_BIT,
      flush: (rclass & FLUSH_BIT) !== 0,
      ttl,
      data,
    });
    pos += rdlength;
  }
  return { records, offset: pos };
}

/** Full packet -> {id, flags, questions, answers, additionals}. Used by tests and
 * by anyone who wants to read what we put on the wire; the responder never needs it. */
export function decodeMessage(buf) {
  const header = parseHeader(buf);
  if (!header) return null;
  const questions = parseQuestions(buf);

  let offset = 12;
  for (let i = 0; i < header.qdcount; i += 1) {
    try {
      const decoded = decodeName(buf, offset);
      offset = decoded.offset + 4;
    } catch { break; }
  }

  const answers = decodeRecords(buf, offset, header.ancount);
  const authorities = decodeRecords(buf, answers.offset, header.nscount);
  const additionals = decodeRecords(buf, authorities.offset, header.arcount);

  return {
    id: header.id,
    flags: header.flags,
    isResponse: (header.flags & QR_BIT) !== 0,
    questions,
    answers: answers.records,
    authorities: authorities.records,
    additionals: additionals.records,
  };
}

// ------------------------------------------------------- the advertisement

const IPV4 = /^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/;
function isIPv4(address) {
  if (!IPV4.test(String(address))) return false;
  return String(address).split('.').every(o => Number(o) <= 255);
}

/** A DNS label cannot contain a dot or exceed 63 bytes. Instance names are human
 * strings, so a space is legal and kept ("Fleet Deck" is a valid DNS-SD instance
 * name, RFC 6763 §4.1.1); a dot becomes a dash rather than silently splitting one
 * label into two, and control bytes are never put on the wire. Truncation is
 * byte-wise, so drop a half-eaten multibyte tail. */
function label(value, fallback) {
  const text = String(value ?? '').replace(/[.\u0000-\u001f\u007f]/g, '-').trim();
  const bytes = Buffer.from(text || fallback, 'utf8').subarray(0, 63);
  return bytes.toString('utf8').replace(/\ufffd+$/, '') || fallback;
}

/** Options -> the concrete thing we advertise. Pure; safe to call per packet. */
export function normalize(options = {}) {
  const host = `${label(options.name || 'fleetdeck', 'fleetdeck')}.local`;
  const instance = label(options.instance || 'Fleet Deck', 'Fleet Deck');
  const port = Number(options.port) || 0;
  const addresses = (Array.isArray(options.addresses) ? options.addresses : []).filter(isIPv4);
  const txt = { path: '/', board: 'fleetdeck', ...(options.txt || {}) };
  const services = SERVICE_TYPES.map(type => ({ type, name: `${instance}.${type}` }));
  return { host, instance, port, addresses, txt, services };
}

function ttlFor(type, override) {
  return override === undefined ? DEFAULT_TTL[type] : override;
}

function aRecords(ad, ttl, flush) {
  return ad.addresses.map(address => ({
    name: ad.host, type: 'A', ttl: ttlFor('A', ttl), flush, data: address,
  }));
}
function ptrRecord(ad, service, ttl) {
  // PTR is a SHARED record set (other hosts answer the same question) — never flush.
  return { name: service.type, type: 'PTR', ttl: ttlFor('PTR', ttl), flush: false, data: service.name };
}
function srvRecord(ad, service, ttl, flush) {
  return {
    name: service.name, type: 'SRV', ttl: ttlFor('SRV', ttl), flush,
    data: { priority: 0, weight: 0, port: ad.port, target: ad.host },
  };
}
function txtRecord(ad, service, ttl, flush) {
  return { name: service.name, type: 'TXT', ttl: ttlFor('TXT', ttl), flush, data: ad.txt };
}
function metaRecords(ad, ttl) {
  return ad.services.map(service => ({
    name: META_QUERY, type: 'PTR', ttl: ttlFor('PTR', ttl), flush: false, data: service.type,
  }));
}

function keyOf(record) {
  return `${String(record.name).toLowerCase()}|${record.type}|${JSON.stringify(record.data)}`;
}

/** Everything we own, in one Answer section: what an unsolicited announcement
 * carries (RFC 6762 §8.3) and — with `ttl: 0` — what a goodbye carries (§10.1). */
export function buildAnnouncement(options = {}, { ttl } = {}) {
  const ad = normalize(options);
  const flush = ttl !== 0; // a goodbye must not also claim uniqueness
  const records = [
    ...aRecords(ad, ttl, flush),
    ...metaRecords(ad, ttl),
  ];
  for (const service of ad.services) {
    records.push(ptrRecord(ad, service, ttl), srvRecord(ad, service, ttl, flush), txtRecord(ad, service, ttl, flush));
  }
  const seen = new Set();
  return records.filter(r => !seen.has(keyOf(r)) && seen.add(keyOf(r)));
}

/** Questions -> the records we owe them. Names match case-insensitively (RFC 6762
 * §16). Anything we do not own — a neighbour's hostname, AAAA, a service type that
 * is not ours — produces no answer at all, and no answer means we stay silent. */
export function buildResponse(questions, options = {}, { ttl, flush = true } = {}) {
  const ad = normalize(options);
  const answers = [];
  const additionals = [];

  const wants = (q, type) => q.type === TYPE.ANY || q.type === TYPE[type];

  for (const q of Array.isArray(questions) ? questions : []) {
    if (q.class && q.class !== CLASS_IN && q.class !== CLASS_ANY) continue; // not our class
    const qname = String(q.name || '').replace(/\.$/, '').toLowerCase();

    if (qname === ad.host.toLowerCase() && wants(q, 'A')) {
      answers.push(...aRecords(ad, ttl, flush));
    }

    if (qname === META_QUERY && wants(q, 'PTR')) {
      answers.push(...metaRecords(ad, ttl));
    }

    for (const service of ad.services) {
      if (qname === service.type.toLowerCase() && wants(q, 'PTR')) {
        answers.push(ptrRecord(ad, service, ttl));
        // RFC 6763 §12.1: SRV + TXT + A alongside the PTR, so the browser resolves
        // us in ONE round-trip instead of three.
        additionals.push(
          srvRecord(ad, service, ttl, flush),
          txtRecord(ad, service, ttl, flush),
          ...aRecords(ad, ttl, flush),
        );
      }
      if (qname === service.name.toLowerCase()) {
        if (wants(q, 'SRV')) {
          answers.push(srvRecord(ad, service, ttl, flush));
          additionals.push(...aRecords(ad, ttl, flush)); // §12.2: the target's address
        }
        if (wants(q, 'TXT')) answers.push(txtRecord(ad, service, ttl, flush));
      }
    }
  }

  const answerKeys = new Set();
  const dedupedAnswers = answers.filter(r => !answerKeys.has(keyOf(r)) && answerKeys.add(keyOf(r)));
  const extraKeys = new Set();
  const dedupedAdditionals = additionals.filter(r =>
    !answerKeys.has(keyOf(r)) && !extraKeys.has(keyOf(r)) && extraKeys.add(keyOf(r)));

  return { answers: dedupedAnswers, additionals: dedupedAdditionals };
}

// ------------------------------------------------------------- the responder

/**
 * @param {object} opts
 * @param {number} opts.port        the board's HTTP port (advertised in SRV)
 * @param {string} [opts.name]      host label; 'fleetdeck' -> fleetdeck.local
 * @param {string} [opts.instance]  human service instance name
 * @param {string[]} [opts.addresses] LAN IPv4s to advertise (non-internal only)
 * @param {object} [opts.txt]       extra TXT keys, merged over {path, board}
 * @param {function} [opts.log]
 * @returns {{start: function, stop: function}} both idempotent, neither throws
 */
export function createMdns({ port, name = 'fleetdeck', instance = 'Fleet Deck', addresses = [], txt, log = () => {} } = {}) {
  const options = { port, name, instance, addresses, txt };
  const ad = normalize(options);

  let socket = null;
  let started = false;
  let stopping = null;
  let dead = false;
  const timers = new Set();

  const note = (message) => { try { log(message); } catch { /* a broken logger must not kill mDNS */ } };

  // One-way door: every failure lands here, says why once, and leaves the daemon alone.
  function die(reason, err) {
    if (dead) return;
    dead = true;
    note(`mdns disabled (${reason})${err && err.message ? `: ${err.message}` : ''} — the board still works over its IP`);
    for (const t of timers) clearTimeout(t);
    timers.clear();
    const doomed = socket;
    socket = null;
    try { doomed?.close(); } catch { /* already closed */ }
  }

  function send(packet, targetPort, targetAddress) {
    if (!socket || dead) return;
    try {
      socket.send(packet, targetPort, targetAddress, (err) => {
        // A send error is per-packet, not fatal: the LAN may just have no route
        // right now. Log at most once by routing repeat failures through die().
        if (err && err.code !== 'ENETUNREACH' && err.code !== 'EHOSTUNREACH') note(`mdns send failed: ${err.message}`);
      });
    } catch (err) {
      note(`mdns send failed: ${err.message}`);
    }
  }

  function announce(ttl) {
    try {
      const answers = buildAnnouncement(options, ttl === undefined ? {} : { ttl });
      if (!answers.length) return;
      send(encodeMessage({ id: 0, flags: FLAGS_RESPONSE, answers }), MDNS_PORT, MDNS_ADDR);
    } catch (err) {
      note(`mdns announce failed: ${err.message}`);
    }
  }

  function onMessage(msg, rinfo) {
    try {
      const header = parseHeader(msg);
      if (!header || header.qdcount === 0) return;
      if ((header.flags & QR_BIT) !== 0) return; // a response, including our own echo

      const questions = parseQuestions(msg);
      if (!questions.length) return;

      // RFC 6762 §6.7: a source port other than 5353 is a legacy unicast resolver.
      const legacy = rinfo.port !== MDNS_PORT;
      const { answers, additionals } = legacy
        ? buildResponse(questions, options, { ttl: LEGACY_TTL, flush: false })
        : buildResponse(questions, options);
      if (!answers.length) return; // not ours — stay silent

      const packet = encodeMessage({
        id: legacy ? header.id : 0,
        flags: FLAGS_RESPONSE,
        questions: legacy ? questions : [], // legacy resolvers match on the echoed question
        answers,
        additionals,
      });

      if (legacy || questions.some(q => q.unicast)) send(packet, rinfo.port, rinfo.address);
      else send(packet, MDNS_PORT, MDNS_ADDR);
    } catch (err) {
      note(`mdns query handling error: ${err.message}`); // a stranger's junk packet
    }
  }

  function onBound() {
    try { socket.setMulticastTTL(255); } catch { /* not fatal; kernel default is 1 */ }
    try { socket.setMulticastLoopback(true); } catch { /* not fatal */ }

    // Join on the default interface, then per-address so a multi-homed host (and
    // WSL2's mirrored stack) actually receives on the LAN interface. Per-interface
    // failures are expected and swallowed — one successful join is enough.
    let joins = 0;
    try { socket.addMembership(MDNS_ADDR); joins += 1; } catch { /* no default multicast route */ }
    for (const address of ad.addresses) {
      try { socket.addMembership(MDNS_ADDR, address); joins += 1; } catch { /* already joined via this iface, or it has no multicast */ }
    }
    if (joins === 0) { die('no multicast membership'); return; }

    for (const delay of ANNOUNCE_DELAYS_MS) {
      const timer = setTimeout(() => { timers.delete(timer); announce(); }, delay);
      timer.unref?.(); // mDNS must never hold the daemon's event loop open
      timers.add(timer);
    }
    note(`mdns responding for ${ad.host}:${ad.port}${ad.addresses.length ? ` (${ad.addresses.join(', ')})` : ' (no LAN address to advertise)'}`);
  }

  function start() {
    if (started || dead) return;
    started = true;

    if (!ad.port) { die('no port to advertise'); return; }
    if (!ad.addresses.length) {
      // Nothing to put in an A record; SRV would point at a name that resolves to
      // nothing. Better to say so once than to advertise a dead host.
      die('no non-internal IPv4 address');
      return;
    }

    try {
      socket = dgram.createSocket({ type: 'udp4', reuseAddr: true });
    } catch (err) {
      die('socket create failed', err);
      return;
    }

    // EADDRINUSE here means a real responder (avahi, Bonjour) already owns 5353 —
    // which is FINE: it will answer for the host anyway. We simply stand down.
    socket.on('error', err => die(err.code === 'EADDRINUSE' ? 'port 5353 already owned by another responder' : (err.code || 'socket error'), err));
    socket.on('message', onMessage);

    try {
      socket.bind({ port: MDNS_PORT }, () => { try { onBound(); } catch (err) { die('bind setup failed', err); } });
    } catch (err) {
      die('bind failed', err);
    }
  }

  /** Goodbye (TTL 0, RFC 6762 §10.1) then close. Returns a promise that always
   * resolves — a shutdown path must not be able to reject. */
  function stop() {
    if (stopping) return stopping;

    // Never started, already degraded, or already closed: there is nothing to say
    // goodbye with and nothing to close. Go quiet WITHOUT logging — stop() is on
    // the daemon's signal path and a no-op shutdown is not news.
    if (!started || dead || !socket) {
      dead = true;
      for (const t of timers) clearTimeout(t);
      timers.clear();
      stopping = Promise.resolve();
      return stopping;
    }

    for (const t of timers) clearTimeout(t);
    timers.clear();

    stopping = new Promise((resolve) => {
      const finish = () => {
        const doomed = socket;
        socket = null;
        dead = true;
        try { doomed?.close(resolve); } catch { resolve(); }
      };
      try {
        const answers = buildAnnouncement(options, { ttl: 0 });
        const packet = encodeMessage({ id: 0, flags: FLAGS_RESPONSE, answers });
        // Close on the send callback, but never wait forever on a wedged socket.
        const guard = setTimeout(finish, 250);
        guard.unref?.();
        socket.send(packet, MDNS_PORT, MDNS_ADDR, () => { clearTimeout(guard); finish(); });
      } catch {
        finish();
      }
    });
    return stopping;
  }

  return { start, stop };
}
