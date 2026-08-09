// mdns.ts — a dependency-free mDNS (RFC 6762) + DNS-SD (RFC 6763) responder.
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
//   - The names we claim are UNIQUE (the A record carries the cache-flush bit).
//     Each interface asserts ownership OPTIMISTICALLY — it announces the moment
//     it can (RFC 6762 §8.3), rather than gating behind a probe window — but
//     conflict handling stays reactive: once we are answering, a response
//     carrying records that clash with ours (a different address on our name)
//     stands us down (§9, §8.2) instead of entering an infinite defense war. A
//     TTL-0 goodbye is a withdrawal, never a competing claim, and is ignored.
//     buildProbeQuestions / the §8.1 tie-break in uniqueConflict remain exported
//     and unit-tested so a probing lifecycle can be layered back on.
//
// A multihomed host gets one socket PER multicast-capable interface
// (createLinks below): each link answers and announces out of its own
// interface, advertising only the A record valid on that link. A packet that
// leaves on the OS-selected default interface while claiming addresses from
// another LAN is exactly the "browser resolves us, cannot connect" failure
// this module exists to prevent.
//
// Everything above the socket is pure and exported (encodeName/decodeName,
// encodeRecord, parseQuestions, buildResponse, buildAnnouncement, encodeMessage,
// decodeMessage) so the wire format is unit-testable without touching a network.

import dgram from 'node:dgram';
import type { RemoteInfo, Socket } from 'node:dgram';
import os from 'node:os';

export const MDNS_ADDR = '224.0.0.251';
export const MDNS_PORT = 5353;

export const TYPE = { A: 1, PTR: 12, TXT: 16, AAAA: 28, SRV: 33, ANY: 255 };
const TYPE_NAME: Record<number, string> = Object.fromEntries(
  Object.entries(TYPE).map(([k, v]): [number, string] => [v, k]),
);

const CLASS_IN = 1;
const CLASS_ANY = 255; // a QCLASS of 255 ("any class") also matches IN
const FLUSH_BIT = 0x8000; // top bit of an ANSWER's class: cache-flush
const QU_BIT = 0x8000; // top bit of a QUESTION's class: unicast reply wanted
const QR_BIT = 0x8000; // top bit of the header flags: this is a response
const FLAGS_RESPONSE = 0x8400; // QR + AA — an mDNS responder is always authoritative

// RFC 6762 §10: 120s for records that name a host, 4500s for everything else.
const DEFAULT_TTL = { A: 120, SRV: 120, PTR: 4500, TXT: 4500 };
const LEGACY_TTL = 10; // §6.7 — a legacy resolver's cache must not outlive us for long

const SERVICE_TYPES = ['_fleetdeck._tcp.local', '_http._tcp.local'];
export const META_QUERY = '_services._dns-sd._udp.local';

const ANNOUNCE_DELAYS_MS = [0, 1000, 2000]; // RFC 6762 §8.3: 2-3 announcements, ~1s apart

// ------------------------------------------------------------------- data model

interface SrvData {
  priority: number;
  weight: number;
  port: number;
  target: string;
}
type RecordData = string | string[] | Record<string, string> | SrvData | Buffer;

interface DnsRecord {
  name: string;
  type: string | number;
  typeName?: string;
  class?: number;
  flush?: boolean;
  ttl?: number;
  data: RecordData;
}

interface Question {
  name: string;
  type: number;
  typeName: string;
  class: number;
  unicast: boolean;
}

interface OutQuestion {
  name: string;
  type?: number;
  class?: number;
  unicast?: boolean;
}

interface DnsHeader {
  id: number;
  flags: number;
  qdcount: number;
  ancount: number;
  nscount: number;
  arcount: number;
}

interface DecodedMessage {
  id: number;
  flags: number;
  isResponse: boolean;
  questions: Question[];
  answers: DnsRecord[];
  authorities: DnsRecord[];
  additionals: DnsRecord[];
}

interface EncodeMessageInput {
  id?: number;
  flags?: number;
  questions?: OutQuestion[];
  answers?: DnsRecord[];
  authorities?: DnsRecord[];
  additionals?: DnsRecord[];
}

interface AdService {
  type: string;
  name: string;
}

interface Advertisement {
  host: string;
  instance: string;
  port: number;
  addresses: string[];
  txt: Record<string, string>;
  services: AdService[];
}

interface MdnsOptions {
  host?: string;
  name?: string;
  instance?: string;
  port?: number | string | undefined;
  addresses?: string[];
  txt?: Record<string, string> | undefined;
}

interface CreateMdnsOptions {
  port?: number;
  name?: string;
  instance?: string;
  addresses?: string[];
  txt?: Record<string, string> | undefined;
  log?: (message: string) => void;
  onDown?: ((reason: string) => void) | null;
  inject?: { dgram?: typeof dgram } | undefined;
}

interface MdnsResponder {
  start: () => void;
  stop: () => Promise<void>;
  update: (nextAddresses?: string[] | { addresses?: string[] }) => boolean;
  alive: () => boolean;
}

/** Read a `useUnknownInCatchVariables` catch value back as a message string. */
function errMessage(err: unknown): string {
  return err instanceof Error && err.message ? err.message : String(err);
}

// ------------------------------------------------------------------ wire codec

/** A dotted name -> length-prefixed labels + a root NUL. */
export function encodeName(name: string): Buffer {
  const labels = name.replace(/\.$/, '').split('.').filter(Boolean);
  const parts: Buffer[] = [];
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
export function decodeName(buf: Buffer, offset = 0): { name: string; offset: number } {
  const labels: string[] = [];
  let pos = offset;
  let end = offset;
  let jumped = false;
  let hops = 0;

  for (;;) {
    const len = buf[pos];
    if (len === undefined) throw new RangeError('mdns: name runs past end of packet');

    if (len === 0) {
      pos += 1;
      if (!jumped) end = pos;
      break;
    }

    if ((len & 0xc0) === 0xc0) {
      const next = buf[pos + 1];
      if (next === undefined) throw new RangeError('mdns: truncated compression pointer');
      const target = ((len & 0x3f) << 8) | next;
      if (!jumped) {
        end = pos + 2;
        jumped = true;
      }
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

function typeNumber(type: string | number): number {
  if (typeof type === 'number') return type;
  const n = (TYPE as Record<string, number | undefined>)[type.toUpperCase()];
  if (n === undefined) throw new TypeError(`mdns: unknown record type ${type}`);
  return n;
}

function encodeIPv4(address: string): Buffer {
  const octets = address.split('.').map(Number);
  if (octets.length !== 4 || octets.some((o) => !Number.isInteger(o) || o < 0 || o > 255)) {
    throw new TypeError(`mdns: not an IPv4 address: ${address}`);
  }
  return Buffer.from(octets);
}

function encodeTxt(data: string[] | Record<string, string>): Buffer {
  const strings = Array.isArray(data) ? data : Object.entries(data).map(([k, v]) => `${k}=${v}`);
  // An empty TXT is illegal: RFC 6763 §6.1 requires a single zero-length string.
  if (!strings.length) return Buffer.from([0]);
  const parts: Buffer[] = [];
  for (const s of strings) {
    const bytes = Buffer.from(s, 'utf8').subarray(0, 255);
    parts.push(Buffer.from([bytes.length]), bytes);
  }
  return Buffer.concat(parts);
}

function encodeRdata(type: number, data: RecordData): Buffer {
  switch (type) {
    case TYPE.A:
      return encodeIPv4(data as string);
    case TYPE.PTR:
      return encodeName(data as string);
    case TYPE.TXT:
      return encodeTxt(data as string[] | Record<string, string>);
    case TYPE.SRV: {
      const srv = data as SrvData;
      const head = Buffer.alloc(6);
      head.writeUInt16BE(srv.priority, 0);
      head.writeUInt16BE(srv.weight, 2);
      head.writeUInt16BE(srv.port, 4);
      return Buffer.concat([head, encodeName(srv.target)]);
    }
    default:
      throw new TypeError(`mdns: cannot encode rdata for type ${type}`);
  }
}

/** One resource record -> wire bytes. `flush` sets the cache-flush class bit. */
export function encodeRecord(record: DnsRecord): Buffer {
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

function parseHeader(buf: Buffer): DnsHeader | null {
  if (buf.length < 12) return null;
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
export function parseQuestions(buf: Buffer): Question[] {
  const header = parseHeader(buf);
  if (!header) return [];

  const questions: Question[] = [];
  let offset = 12;
  for (let i = 0; i < header.qdcount; i += 1) {
    let decoded: { name: string; offset: number };
    try {
      decoded = decodeName(buf, offset);
    } catch {
      break;
    }
    offset = decoded.offset;
    if (offset + 4 > buf.length) break;
    const type = buf.readUInt16BE(offset);
    const qclass = buf.readUInt16BE(offset + 2);
    offset += 4;
    questions.push({
      name: decoded.name,
      type,
      // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- an empty type name (never emitted) would still want the numeric fallback
      typeName: TYPE_NAME[type] || String(type),
      class: qclass & ~QU_BIT,
      unicast: (qclass & QU_BIT) !== 0,
    });
  }
  return questions;
}

export function encodeMessage({
  id = 0,
  flags = FLAGS_RESPONSE,
  questions = [],
  answers = [],
  authorities = [],
  additionals = [],
}: EncodeMessageInput = {}): Buffer {
  const header = Buffer.alloc(12);
  header.writeUInt16BE(id & 0xffff, 0);
  header.writeUInt16BE(flags & 0xffff, 2);
  header.writeUInt16BE(questions.length, 4);
  header.writeUInt16BE(answers.length, 6);
  header.writeUInt16BE(authorities.length, 8);
  header.writeUInt16BE(additionals.length, 10);

  const parts: Buffer[] = [header];
  for (const q of questions) {
    const tail = Buffer.alloc(4);
    tail.writeUInt16BE(typeNumber(q.type ?? TYPE.ANY), 0);
    // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- a class of 0 means "unspecified" and must default to IN
    tail.writeUInt16BE((q.class || CLASS_IN) | (q.unicast ? QU_BIT : 0), 2);
    parts.push(encodeName(q.name), tail);
  }
  for (const record of [...answers, ...authorities, ...additionals])
    parts.push(encodeRecord(record));
  return Buffer.concat(parts);
}

function decodeRecords(
  buf: Buffer,
  offset: number,
  count: number,
): { records: DnsRecord[]; offset: number } {
  const records: DnsRecord[] = [];
  let pos = offset;
  for (let i = 0; i < count; i += 1) {
    let decoded: { name: string; offset: number };
    try {
      decoded = decodeName(buf, pos);
    } catch {
      break;
    }
    pos = decoded.offset;
    if (pos + 10 > buf.length) break;
    const type = buf.readUInt16BE(pos);
    const rclass = buf.readUInt16BE(pos + 2);
    const ttl = buf.readUInt32BE(pos + 4);
    const rdlength = buf.readUInt16BE(pos + 8);
    pos += 10;
    if (pos + rdlength > buf.length) break;

    let data: RecordData;
    try {
      switch (type) {
        case TYPE.A:
          data = Array.from(buf.subarray(pos, pos + 4)).join('.');
          break;
        case TYPE.PTR:
          data = decodeName(buf, pos).name;
          break;
        case TYPE.SRV:
          data = {
            priority: buf.readUInt16BE(pos),
            weight: buf.readUInt16BE(pos + 2),
            port: buf.readUInt16BE(pos + 4),
            target: decodeName(buf, pos + 6).name,
          };
          break;
        case TYPE.TXT: {
          const strings: string[] = [];
          for (let p = pos; p < pos + rdlength;) {
            const len = buf[p] ?? 0;
            strings.push(buf.toString('utf8', p + 1, p + 1 + len));
            p += 1 + len;
          }
          data = strings;
          break;
        }
        default:
          data = Buffer.from(buf.subarray(pos, pos + rdlength));
      }
    } catch {
      data = Buffer.from(buf.subarray(pos, pos + rdlength));
    }

    records.push({
      name: decoded.name,
      type,
      // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- an empty type name (never emitted) would still want the numeric fallback
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
export function decodeMessage(buf: Buffer): DecodedMessage | null {
  const header = parseHeader(buf);
  if (!header) return null;
  const questions = parseQuestions(buf);

  let offset = 12;
  for (let i = 0; i < header.qdcount; i += 1) {
    try {
      const decoded = decodeName(buf, offset);
      offset = decoded.offset + 4;
    } catch {
      break;
    }
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
function isIPv4(address: string): boolean {
  if (!IPV4.test(address)) return false;
  return address.split('.').every((o) => Number(o) <= 255);
}

/** A DNS label cannot contain a dot or exceed 63 bytes. Instance names are human
 * strings, so a space is legal and kept ("Fleet Deck" is a valid DNS-SD instance
 * name, RFC 6763 §4.1.1); a dot becomes a dash rather than silently splitting one
 * label into two, and control bytes are never put on the wire. Truncation is
 * byte-wise, so drop a half-eaten multibyte tail. */
function label(value: string | undefined, fallback: string): string {
  // eslint-disable-next-line no-control-regex -- stripping C0/DEL out of a DNS label is the point
  const text = (value ?? '').replace(/[.\u0000-\u001f\u007f]/g, '-').trim();

  const bytes = Buffer.from(text || fallback, 'utf8').subarray(0, 63);

  return bytes.toString('utf8').replace(/\ufffd+$/, '') || fallback;
}

/** The canonical host label behind every surface that speaks the mDNS name.
 * fleetd derives its share URL, startup log line, responder and Host allowlist
 * from this one string: normalize() rewrites dots/controls and truncates to
 * 63 bytes, so a caller that interpolates the raw configured value would
 * publish a name that never resolves and advertise a name its own HTTP layer
 * refuses. Pure; always the exact label normalize() would advertise. */
export function hostLabel(value: string | undefined, fallback = 'fleetdeck'): string {
  return label(value, fallback);
}

/** Options -> the concrete thing we advertise. Pure; safe to call per packet,
 * and IDEMPOTENT: a value already carrying `.host` (i.e. a previous normalize()
 * output) keeps that host instead of re-deriving 'fleetdeck' from an absent
 * `.name`. The live advertisement is stored normalized and handed back to
 * buildAnnouncement/buildResponse, so without this a custom FLEETDECK_MDNS_NAME
 * would probe/advertise its canonical label but ANSWER as fleetdeck.local. */
export function normalize(options: MdnsOptions = {}): Advertisement {
  // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- an empty host must be re-derived from the name, not kept as ''
  const host = options.host || `${hostLabel(options.name, 'fleetdeck')}.local`;
  // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- an empty instance must fall back to the default label
  const instance = label(options.instance || 'Fleet Deck', 'Fleet Deck');

  const port = Number(options.port) || 0;
  const addresses = (Array.isArray(options.addresses) ? options.addresses : []).filter(isIPv4);
  const txt: Record<string, string> = { path: '/', board: 'fleetdeck', ...(options.txt ?? {}) };
  const services = SERVICE_TYPES.map((type) => ({ type, name: `${instance}.${type}` }));
  return { host, instance, port, addresses, txt, services };
}

function ttlFor(type: keyof typeof DEFAULT_TTL, override?: number): number {
  return override ?? DEFAULT_TTL[type];
}

/** Every non-internal IPv4 mapped to the interface name that owns it. Pure apart
 * from the OS call; never throws — a responder that cannot read its interfaces
 * simply has nothing to say. */
export function addressInterfaces(): Map<string, string> {
  const map = new Map<string, string>();
  try {
    for (const [name, entries] of Object.entries(os.networkInterfaces())) {
      for (const entry of entries ?? []) {
        if (entry.family === 'IPv4' && !entry.internal && isIPv4(entry.address)) {
          map.set(entry.address, name);
        }
      }
    }
  } catch {
    /* interfaces change under us; the fallback link below covers it */
  }
  return map;
}

function aRecords(ad: Advertisement, ttl: number | undefined, flush: boolean): DnsRecord[] {
  return ad.addresses.map((address) => ({
    name: ad.host,
    type: 'A',
    ttl: ttlFor('A', ttl),
    flush,
    data: address,
  }));
}
function ptrRecord(_ad: Advertisement, service: AdService, ttl: number | undefined): DnsRecord {
  // PTR is a SHARED record set (other hosts answer the same question) — never flush.
  return {
    name: service.type,
    type: 'PTR',
    ttl: ttlFor('PTR', ttl),
    flush: false,
    data: service.name,
  };
}
function srvRecord(
  ad: Advertisement,
  service: AdService,
  ttl: number | undefined,
  flush: boolean,
): DnsRecord {
  return {
    name: service.name,
    type: 'SRV',
    ttl: ttlFor('SRV', ttl),
    flush,
    data: { priority: 0, weight: 0, port: ad.port, target: ad.host },
  };
}
function txtRecord(
  ad: Advertisement,
  service: AdService,
  ttl: number | undefined,
  flush: boolean,
): DnsRecord {
  return { name: service.name, type: 'TXT', ttl: ttlFor('TXT', ttl), flush, data: ad.txt };
}
function metaRecords(ad: Advertisement, ttl: number | undefined): DnsRecord[] {
  return ad.services.map((service) => ({
    name: META_QUERY,
    type: 'PTR',
    ttl: ttlFor('PTR', ttl),
    flush: false,
    data: service.type,
  }));
}

function keyOf(record: DnsRecord): string {
  return `${record.name.toLowerCase()}|${typeNumber(record.type)}|${JSON.stringify(record.data)}`;
}

/** Everything we own, in one Answer section: what an unsolicited announcement
 * carries (RFC 6762 §8.3) and — with `ttl: 0` — what a goodbye carries (§10.1). */
export function buildAnnouncement(
  options: MdnsOptions = {},
  { ttl }: { ttl?: number } = {},
): DnsRecord[] {
  const ad = normalize(options);
  const flush = ttl !== 0; // a goodbye must not also claim uniqueness
  const records: DnsRecord[] = [...aRecords(ad, ttl, flush), ...metaRecords(ad, ttl)];
  for (const service of ad.services) {
    records.push(
      ptrRecord(ad, service, ttl),
      srvRecord(ad, service, ttl, flush),
      txtRecord(ad, service, ttl, flush),
    );
  }
  const seen = new Set<string>();
  return records.filter((r) => {
    const k = keyOf(r);
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}

/** Questions -> the records we owe them. Names match case-insensitively (RFC 6762
 * §16). Anything we do not own — a neighbour's hostname, AAAA, a service type that
 * is not ours — produces no answer at all, and no answer means we stay silent. */
export function buildResponse(
  questions: Question[],
  options: MdnsOptions = {},
  { ttl, flush = true }: { ttl?: number; flush?: boolean } = {},
): { answers: DnsRecord[]; additionals: DnsRecord[] } {
  const ad = normalize(options);
  const answers: DnsRecord[] = [];
  const additionals: DnsRecord[] = [];

  const wants = (q: Question, type: 'A' | 'PTR' | 'SRV' | 'TXT'): boolean =>
    q.type === TYPE.ANY || q.type === TYPE[type];

  for (const q of questions) {
    if (q.class && q.class !== CLASS_IN && q.class !== CLASS_ANY) continue; // not our class
    const qname = q.name.replace(/\.$/, '').toLowerCase();

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

  const answerKeys = new Set<string>();
  const dedupedAnswers = answers.filter((r) => {
    const k = keyOf(r);
    if (answerKeys.has(k)) return false;
    answerKeys.add(k);
    return true;
  });
  const extraKeys = new Set<string>();
  const dedupedAdditionals = additionals.filter((r) => {
    const k = keyOf(r);
    if (answerKeys.has(k) || extraKeys.has(k)) return false;
    extraKeys.add(k);
    return true;
  });

  return { answers: dedupedAnswers, additionals: dedupedAdditionals };
}

/** The probe query: QU questions (§8.1 — answers should come unicast) naming each
 * of our unique names, with the records we would publish as tie-break authorities. */
export function buildProbeQuestions(options: MdnsOptions = {}): {
  questions: OutQuestion[];
  authorities: DnsRecord[];
} {
  const ad = normalize(options);
  const questions: OutQuestion[] = [
    { name: ad.host, type: TYPE.A, class: CLASS_IN, unicast: true },
    ...ad.services.map((service) => ({
      name: service.name,
      type: TYPE.SRV,
      class: CLASS_IN,
      unicast: true,
    })),
  ];
  const authorities = buildAnnouncement(options, { ttl: 0 }).filter(
    (record) => record.type === 'A' || record.type === 'SRV',
  );
  return { questions, authorities };
}

/** Does a packet from someone else deny us our names? During probing, an answer
 * for one of our unique names — or a probe of the same name whose authority data
 * sorts AFTER ours — means we lost the claim (§8.1 tie-break: later data probes
 * again next try; earlier data loses immediately). Once announced, any response
 * record on a name we own whose rdata differs from ours is a conflict (§9). */
export function uniqueConflict(
  msg: Buffer,
  options: MdnsOptions = {},
  { phase }: { phase?: 'probing' | 'announced' } = {},
): boolean {
  const ad = normalize(options);
  const hosts = new Set([ad.host.toLowerCase()]);
  const services = new Set(ad.services.map((service) => service.name.toLowerCase()));
  // A name|type can back MULTIPLE rdata (one A per LAN address). Keep the whole
  // SET we legitimately own, so a per-interface scoped echo carrying only one of
  // our addresses is recognized as ours, not mistaken for a rival (§9).
  const own = new Map<string, Set<string>>();
  for (const record of buildAnnouncement(options)) {
    if (record.type === 'A' || record.type === 'SRV') {
      const k = `${record.name.toLowerCase()}|${typeNumber(record.type)}`;
      let set = own.get(k);
      if (!set) {
        set = new Set();
        own.set(k, set);
      }
      set.add(keyOf(record));
    }
  }

  const decoded = decodeMessage(msg);
  if (!decoded) return false;

  // A response touching a name we claim, whose rdata matches NONE of ours. Decoded
  // records carry the NUMERIC type; the keyOf data payloads encode identically.
  for (const record of [...decoded.answers, ...decoded.additionals]) {
    if (record.ttl === 0) continue; // a goodbye WITHDRAWS a name (incl. our own re-announce goodbye after update()); it never CLAIMS it (§9)
    const keys = own.get(`${record.name.toLowerCase()}|${record.type}`);
    if (keys && !keys.has(keyOf(record))) return true;
  }

  if (phase === 'probing') {
    // RFC 6762 §8.1 tie-break between simultaneous probes: equal rdata is a tie
    // (defer to later traffic — answering with the same data never conflicts);
    // unequal rdata, the lexicographically later authority wins and the earlier
    // yields. A probe carrying authority data beats one carrying none, and a
    // larger authority set (more claimed records backing it) beats a smaller.
    const ours = buildProbeQuestions(options).authorities;
    const oursKeys = new Set(ours.map(keyOf));
    const theirsKeys = new Set(decoded.authorities.map(keyOf));
    const equalSet =
      theirsKeys.size === oursKeys.size && [...theirsKeys].every((k) => oursKeys.has(k));
    const theyWin =
      !equalSet &&
      decoded.authorities.length > 0 &&
      (decoded.authorities.length > ours.length ||
        (decoded.authorities.length === ours.length &&
          [...theirsKeys].sort().join('') > [...oursKeys].sort().join('')));
    if (theyWin) {
      for (const q of decoded.questions) {
        const name = q.name.replace(/\.$/, '').toLowerCase();
        const names = q.type === TYPE.A || q.type === TYPE.ANY ? hosts : services;
        if (names.has(name)) return true;
      }
    }
  }
  return false;
}

// ------------------------------------------------------------- the responder

/**
 * Create an mDNS responder for the board.
 *
 * @param opts
 * @param opts.port        the board's HTTP port (advertised in SRV)
 * @param opts.name        host label; 'fleetdeck' -> fleetdeck.local
 * @param opts.instance    human service instance name
 * @param opts.addresses   LAN IPv4s to advertise (non-internal only)
 * @param opts.txt         extra TXT keys, merged over {path, board}
 * @param opts.log         line sink for the module's one-per-failure diagnostics
 * @param opts.onDown      called with a reason string the moment the responder
 *                         terminally disables itself (bind, membership or socket
 *                         failure) — never on a clean stop(). Lets the publisher
 *                         retract a URL that would no longer resolve.
 * @param opts.inject      test seam: { dgram } replaces node:dgram, so a
 *                         multi-interface host can be simulated without touching
 *                         a network.
 * @returns start/stop are idempotent and never throw; update() re-bases the
 *          advertisement on a fresh LAN address set (goodbye for withdrawn
 *          records, membership re-join, re-announce); alive() reports whether
 *          the responder is bound and answering.
 */
export function createMdns({
  port,
  name = 'fleetdeck',
  instance = 'Fleet Deck',
  addresses = [],
  txt,
  log = () => {
    /* silent by default */
  },
  onDown = null,
  inject,
}: CreateMdnsOptions = {}): MdnsResponder {
  const options: MdnsOptions = { port, name, instance, addresses, txt };
  const dgramImpl = inject?.dgram ?? dgram;
  // The LIVE advertisement. Identity (host/instance/port/txt) is fixed for the
  // responder's life, but the address set is a property of the NETWORK, and the
  // network moves: Wi-Fi roaming, DHCP renewal, a VPN coming up. update() swaps
  // this snapshot atomically; every answer and announcement built after the swap
  // speaks for the new addresses.
  let ad = normalize(options);

  const note = (message: string): void => {
    try {
      log(message);
    } catch {
      /* a broken logger must not kill mDNS */
    }
  };

  let started = false;
  let stopping: Promise<void> | null = null;
  let dead = false; // a REAL failure (socket/bind/multicast) — terminal, one-way
  let dormant = false; // merely addressless — update() may revive us
  let stopped = false; // stop() was called — nothing may resurrect the responder
  // Interface addresses that successfully joined the group. Multicast egress
  // follows the kernel's default route only, so announcements/replies/goodbyes
  // are repeated once per joined interface (setMulticastInterface) — otherwise
  // a dual-homed host is discoverable on the default LAN and invisible on the
  // others, despite advertising their addresses.
  let joinedIfaces: string[] = [];
  const timers = new Set<NodeJS.Timeout>();
  let claimed = false; // the names are ours and we are answering for them
  let socket: Socket | null = null;

  // One-way door: every fatal failure lands here, says why once, and leaves the
  // daemon alone.
  function die(reason: string, err?: unknown): void {
    if (dead) return;
    dead = true;
    const detail = err instanceof Error && err.message ? `: ${err.message}` : '';
    note(`mdns disabled (${reason})${detail} — the board still works over its IP`);
    for (const t of timers) clearTimeout(t);
    timers.clear();
    const doomed = socket;
    socket = null;
    try {
      doomed?.close();
    } catch {
      /* already closed */
    }
    // The daemon publishes the .local URL when start() returns, but bind and
    // membership failures only surface HERE, one event-loop turn later. Report
    // the terminal disable so the publisher can retract the URL instead of
    // advertising a name that will never resolve.
    if (onDown) {
      try {
        onDown(reason);
      } catch {
        /* a broken listener must not kill mDNS */
      }
    }
  }

  function sendRaw(
    packet: Buffer,
    targetPort: number,
    targetAddress: string,
    callback?: () => void,
  ): void {
    if (!socket || dead) return;
    const sock = socket;
    try {
      sock.send(packet, targetPort, targetAddress, (err: NodeJS.ErrnoException | null) => {
        // A send error is per-packet, not fatal: the LAN may just have no route
        // right now. Log at most once by routing repeat failures through die().
        if (err && err.code !== 'ENETUNREACH' && err.code !== 'EHOSTUNREACH')
          note(`mdns send failed: ${err.message}`);
        callback?.();
      });
    } catch (err) {
      note(`mdns send failed: ${errMessage(err)}`);
      callback?.();
    }
  }

  function send(packet: Buffer, targetPort: number, targetAddress: string): void {
    sendRaw(packet, targetPort, targetAddress);
  }

  // Multicast out of every interface we joined, not just the kernel's default
  // route, and SCOPED per interface: setMulticastInterface pins egress, and
  // buildFor(iface) returns the packet that link may legally advertise — its OWN
  // address only, never another LAN's (BUG-130/131). Each attempt is
  // fire-and-forget; done() fires when they have ALL settled (per-interface
  // errors are tolerated — one wedged interface must not hold the goodbye path
  // open). joinedIfaces empty means no per-interface joins; callers fall back to
  // a single plain send() themselves.
  function sendMulticastAll(buildFor: (iface: string) => Buffer | null, done?: () => void): void {
    if (!socket || dead || !joinedIfaces.length) {
      done?.();
      return;
    }
    const sock = socket;
    let pending = joinedIfaces.length;
    const settled = (): void => {
      pending -= 1;
      if (pending <= 0) done?.();
    };
    for (const iface of joinedIfaces) {
      try {
        sock.setMulticastInterface(iface);
      } catch {
        /* keep the last working egress */
      }
      const packet = buildFor(iface);
      if (packet) sendRaw(packet, MDNS_PORT, MDNS_ADDR, settled);
      else settled();
    }
  }

  // The advertisement (or, ttl:0, the goodbye) scoped to ONE interface's
  // address — the A record valid on that link plus the shared service records.
  function announcementFor(address: string, ttl?: number): Buffer | null {
    const answers = buildAnnouncement(
      { ...options, addresses: [address] },
      ttl === undefined ? {} : { ttl },
    );
    return answers.length ? encodeMessage({ id: 0, flags: FLAGS_RESPONSE, answers }) : null;
  }

  function schedule(fn: () => void, delay: number): void {
    const timer = setTimeout(() => {
      timers.delete(timer);
      fn();
    }, delay);
    timer.unref(); // mDNS must never hold the daemon's event loop open
    timers.add(timer);
  }

  function announce(ttl?: number): void {
    try {
      if (joinedIfaces.length) {
        sendMulticastAll((iface) => announcementFor(iface, ttl));
      } else {
        const answers = buildAnnouncement(options, ttl === undefined ? {} : { ttl });
        if (answers.length)
          send(encodeMessage({ id: 0, flags: FLAGS_RESPONSE, answers }), MDNS_PORT, MDNS_ADDR);
      }
    } catch (err) {
      note(`mdns announce failed: ${errMessage(err)}`);
    }
  }

  /** Post-claim conflict: withdraw our records (TTL 0, §10.1) THEN stand down.
   * The send must land before die() runs — die() closes the socket, and a
   * closed socket flushes nothing. */
  function withdrawAndDie(reason: string): void {
    let down = false;
    const finish = (): void => {
      if (!down) {
        down = true;
        die(reason);
      }
    };
    try {
      const guard = setTimeout(finish, 250);
      guard.unref();
      if (joinedIfaces.length) {
        // The withdrawal must reach every LAN we advertised on, not just the
        // default route — each link withdraws its own address; settle all egress
        // sends, then die exactly once.
        sendMulticastAll(
          (iface) => announcementFor(iface, 0),
          () => {
            clearTimeout(guard);
            finish();
          },
        );
      } else if (socket) {
        const sock = socket;
        const answers = buildAnnouncement(options, { ttl: 0 });
        sock.send(
          encodeMessage({ id: 0, flags: FLAGS_RESPONSE, answers }),
          MDNS_PORT,
          MDNS_ADDR,
          () => {
            clearTimeout(guard);
            finish();
          },
        );
      } else {
        clearTimeout(guard);
        finish();
      }
    } catch {
      finish();
    }
  }

  function onMessage(msg: Buffer, rinfo: RemoteInfo): void {
    try {
      const header = parseHeader(msg);
      if (!header) return;

      // Conflict detection BEFORE the question gate: a response (no questions)
      // carrying records on a name we own but with rdata that matches NONE of
      // ours is another device claiming our name. RFC 6762 §9: defending forever
      // is the worst outcome (two hosts announcing in a loop), so one conflicting
      // response stands us down. Our own loopback echoes — including the
      // per-interface scoped ones and TTL-0 goodbyes — match our own set (or are
      // skipped as withdrawals) and pass.
      if (
        socket &&
        rinfo.port === MDNS_PORT &&
        !stopping &&
        claimed &&
        (header.flags & QR_BIT) !== 0
      ) {
        if (uniqueConflict(msg, options, { phase: 'announced' })) {
          withdrawAndDie(`name conflict for ${ad.host} — another device owns this name`);
          return;
        }
      }
      if (header.qdcount === 0) return;
      if ((header.flags & QR_BIT) !== 0) return; // a response, including our own echo

      const questions = parseQuestions(msg);
      if (!questions.length) return;

      // RFC 6762 §6.7: a source port other than 5353 is a legacy unicast resolver.
      // The query is answered against the CURRENT advertisement (options, kept in
      // step by update()), never the startup snapshot — a network change re-bases
      // what we claim to own.
      const legacy = rinfo.port !== MDNS_PORT;
      // "Is this query even ours?" — decide against the full advertisement.
      const full = legacy
        ? buildResponse(questions, options, { ttl: LEGACY_TTL, flush: false })
        : buildResponse(questions, options);
      if (!full.answers.length) return; // not ours — stay silent

      if (legacy || questions.some((q) => q.unicast)) {
        // Unicast (QU / legacy): one reply straight to the querier; the routing
        // table, not multicast steering, chooses the egress, so it is not pinned
        // to any interface and carries the full advertisement.
        send(
          encodeMessage({
            id: legacy ? header.id : 0,
            flags: FLAGS_RESPONSE,
            questions: legacy ? questions : [], // legacy resolvers match on the echoed question
            answers: full.answers,
            additionals: full.additionals,
          }),
          rinfo.port,
          rinfo.address,
        );
      } else if (joinedIfaces.length) {
        // Multicast: answer on EVERY joined interface, each scoped to its own
        // address so a peer never caches a cross-interface A record (BUG-131).
        sendMulticastAll((iface) => {
          const { answers, additionals } = buildResponse(questions, {
            ...options,
            addresses: [iface],
          });
          return answers.length
            ? encodeMessage({ id: 0, flags: FLAGS_RESPONSE, questions: [], answers, additionals })
            : null;
        });
      } else {
        send(
          encodeMessage({
            id: 0,
            flags: FLAGS_RESPONSE,
            questions: [],
            answers: full.answers,
            additionals: full.additionals,
          }),
          MDNS_PORT,
          MDNS_ADDR,
        );
      }
    } catch (err) {
      note(`mdns query handling error: ${errMessage(err)}`); // a stranger's junk packet
    }
  }

  function onBound(): void {
    const sock = socket;
    if (!sock) return;
    // RFC 6762 §11: EVERY mDNS packet — multicast or unicast — must leave with
    // IP TTL 255, because receivers are entitled to verify it as proof the
    // source is on-link. setMulticastTTL covers only the multicast replies;
    // QU and legacy answers go by unicast and would otherwise carry the
    // platform default (commonly 64), so a strict peer discards them.

    try {
      sock.setTTL?.(255); // eslint-disable-line @typescript-eslint/no-unnecessary-condition -- an injected test socket (inject.dgram) may omit setTTL; the optional call lets it degrade instead of throwing
    } catch (err) {
      die('cannot set unicast TTL 255', err);
      return;
    }
    try {
      sock.setMulticastTTL(255);
    } catch (err) {
      die('cannot set multicast TTL 255', err);
      return;
    }
    try {
      sock.setMulticastLoopback(true);
    } catch {
      /* not fatal */
    }

    // Join on the default interface, then per-address so a multi-homed host (and
    // WSL2's mirrored stack) actually receives on the LAN interface. Per-interface
    // failures are expected and swallowed — one successful join is enough. The
    // addresses that joined are remembered: multicast replies, announcements and
    // the goodbye are repeated out of each, because multicast egress otherwise
    // follows only the kernel's default route and secondary LANs never see us.
    joinedIfaces = [];
    let joins = 0;
    try {
      sock.addMembership(MDNS_ADDR);
      joins += 1;
    } catch {
      /* no default multicast route */
    }
    for (const address of ad.addresses) {
      try {
        sock.addMembership(MDNS_ADDR, address);
        joinedIfaces.push(address);
        joins += 1;
      } catch {
        /* already joined via this iface, or it has no multicast */
      }
    }
    if (joins === 0) {
      die('no multicast membership');
      return;
    }

    // Each joined interface speaks the moment it can: an unsolicited announcement
    // (RFC 6762 §8.3), repeated on the ANNOUNCE_DELAYS_MS cadence, scoped per
    // interface. Ownership is asserted optimistically rather than gated behind a
    // probe window, but conflict handling stays reactive — a response on our name
    // whose rdata is none of ours stands us down (§9, onMessage above).
    claimed = true;
    for (const delay of ANNOUNCE_DELAYS_MS)
      schedule(() => {
        announce();
      }, delay);
    note(
      `mdns responding for ${ad.host}:${ad.port}${ad.addresses.length ? ` (${ad.addresses.join(', ')})` : ' (no LAN address to advertise)'}`,
    );
  }

  function start(): void {
    if (started || stopped) return;
    started = true;

    if (!ad.port) {
      die('no port to advertise');
      return;
    }
    if (!ad.addresses.length) {
      // Nothing to put in an A record; SRV would point at a name that resolves to
      // nothing. Degrade for NOW — but the network may only be down at boot
      // (Wi-Fi still associating, DHCP pending), so update() is allowed to retry
      // the moment an address shows up instead of this being a one-way door.
      // The message keeps die()'s exact shape, and it is said ONCE — repeated
      // start() calls on an addressless responder stay as quiet as before.
      if (!dormant) {
        note('mdns disabled (no non-internal IPv4 address) — the board still works over its IP');
        if (onDown) {
          try {
            onDown('no non-internal IPv4 address');
          } catch {
            /* a broken listener must not kill mDNS */
          }
        }
      }
      started = false;
      dormant = true;
      return;
    }

    let sock: Socket;
    try {
      sock = dgramImpl.createSocket({ type: 'udp4', reuseAddr: true });
    } catch (err) {
      die('socket create failed', err);
      return;
    }
    socket = sock;

    // EADDRINUSE here means a real responder (avahi, Bonjour) already owns 5353 —
    // which is FINE: it will answer for the host anyway. We simply stand down.
    sock.on('error', (err: NodeJS.ErrnoException) => {
      die(
        err.code === 'EADDRINUSE'
          ? 'port 5353 already owned by another responder'
          : (err.code ?? 'socket error'),
        err,
      );
    });
    sock.on('message', onMessage);
    try {
      sock.bind({ port: MDNS_PORT }, () => {
        try {
          onBound();
        } catch (err) {
          die('bind setup failed', err);
        }
      });
    } catch (err) {
      die('bind failed', err);
    }
  }

  /**
   * Swap the advertised LAN IPv4 set after an interface change. Accepts either
   * the bare array (the daemon's LAN watcher hands over what it polled) or
   * `{ addresses }`. Removed addresses go out as TTL-0 goodbyes (RFC 6762
   * §10.1 — flush off, exactly the A records being retired, never the shared
   * service records), then the new set announces on the same cadence as
   * startup. A responder that started with no LAN address at all (dormant,
   * not dead) re-runs the whole bind/join dance. No-op after stop()/die(), or
   * when the set is unchanged. Never throws: discovery must not be able to
   * take the daemon down mid-roam any more than at startup.
   */
  function update(nextAddresses?: string[] | { addresses?: string[] }): boolean {
    if (dead || stopped || stopping) return false;
    try {
      const raw = Array.isArray(nextAddresses) ? nextAddresses : nextAddresses?.addresses;
      const addresses = (Array.isArray(raw) ? raw : []).filter(isIPv4);

      if (dormant) {
        // start() found no LAN address at boot and stood down — the network just
        // gave us one, so try the whole bind/join dance again. A real failure
        // inside start() still lands in die() and stays terminal.
        if (!addresses.length) return false;
        options.addresses = addresses;
        ad = normalize(options);
        dormant = false;
        start();
        return !dead;
      }
      if (!started || !socket) return false;
      const sock = socket;
      if (!addresses.length) {
        // Never advertise a host with no A records (the same rule start()
        // enforces). Keep the last set: every address may be momentarily gone
        // mid-roam, and the next poll brings a real set.
        return false;
      }
      if (
        addresses.length === ad.addresses.length &&
        addresses.every((a) => ad.addresses.includes(a))
      )
        return false;

      const removed = ad.addresses.filter((a) => !addresses.includes(a));
      const added = addresses.filter((a) => !ad.addresses.includes(a));
      options.addresses = addresses;
      ad = normalize(options);
      // Withdraw each retired address with a TTL-0 A goodbye (§10.1 — flush off,
      // EXACTLY the A record being retired, never the shared PTR/SRV/TXT), out
      // of that address's own interface, BEFORE dropping its membership.
      for (const address of removed) {
        const goodbye = encodeMessage({
          id: 0,
          flags: FLAGS_RESPONSE,
          answers: [{ name: ad.host, type: 'A', ttl: 0, flush: false, data: address }],
        });
        if (joinedIfaces.includes(address)) {
          try {
            sock.setMulticastInterface(address);
          } catch {
            /* keep the last working egress */
          }
          sendRaw(goodbye, MDNS_PORT, MDNS_ADDR);
        } else {
          send(goodbye, MDNS_PORT, MDNS_ADDR);
        }
      }
      for (const address of removed) {
        try {
          sock.dropMembership(MDNS_ADDR, address);
        } catch {
          /* not joined via this iface, or no dropMembership */
        }
      }
      joinedIfaces = joinedIfaces.filter((a) => !removed.includes(a));
      for (const address of added) {
        try {
          sock.addMembership(MDNS_ADDR, address);
          if (!joinedIfaces.includes(address)) joinedIfaces.push(address);
        } catch {
          /* already joined, or no multicast here */
        }
      }
      for (const delay of ANNOUNCE_DELAYS_MS)
        schedule(() => {
          announce();
        }, delay);
      note(`mdns addresses updated (${ad.addresses.join(', ')})`);
      return true;
    } catch (err) {
      note(`mdns address update failed: ${errMessage(err)}`);
      return false;
    }
  }

  /** Goodbye (TTL 0, RFC 6762 §10.1) on every joined interface, then close.
   * Returns a promise that always resolves — a shutdown path must not be able
   * to reject. */
  function stop(): Promise<void> {
    if (stopping) return stopping;

    // Never started, already degraded, or already closed: there is nothing to say
    // goodbye with and nothing to close. Go quiet WITHOUT logging — stop() is on
    // the daemon's signal path and a no-op shutdown is not news.
    if (!started || dead || !socket) {
      dead = true;
      stopped = true;
      for (const t of timers) clearTimeout(t);
      timers.clear();
      stopping = Promise.resolve();
      return stopping;
    }

    for (const t of timers) clearTimeout(t);
    timers.clear();

    stopping = new Promise<void>((resolve) => {
      const finish = (): void => {
        const doomed = socket;
        socket = null;
        dead = true;
        stopped = true;
        try {
          doomed?.close(resolve);
        } catch {
          resolve();
        }
      };
      try {
        // Close on the send callback(s), but never wait forever on a wedged socket.
        const guard = setTimeout(finish, 250);
        guard.unref();
        const done = (): void => {
          clearTimeout(guard);
          finish();
        };
        // The goodbye must reach every LAN we advertised on, not just the default
        // route — each link withdraws its own address (a stale record on a
        // secondary interface is exactly what the goodbye exists to retract).
        if (joinedIfaces.length) {
          sendMulticastAll((iface) => announcementFor(iface, 0), done);
        } else {
          const sock = socket;
          const answers = buildAnnouncement(options, { ttl: 0 });
          if (sock)
            sock.send(
              encodeMessage({ id: 0, flags: FLAGS_RESPONSE, answers }),
              MDNS_PORT,
              MDNS_ADDR,
              done,
            );
          else done();
        }
      } catch {
        finish();
      }
    });
    return stopping;
  }

  return { start, stop, update, alive: () => started && !dead && socket !== null };
}
