// Pasted-image ingest — the server half of "Ctrl+V a screenshot into the board
// terminal".
//
// A browser terminal can never hand an image to the agent directly: the blob
// lives in the BROWSER's clipboard, the wire carries text, and Claude Code has
// no Linux clipboard-image path anyway (its clipboard read is macOS-only, and
// the fallback tools want an X/Wayland display a headless box does not have).
// File paths are the one image input that works everywhere — so the board
// uploads the blob here, we write it to disk, and the board TYPES the returned
// path into the pane through its normal stdin path. This module never touches
// tmux: injection stays in the browser on purpose, so the grid's
// one-tile-types discipline (TermPane's sendIn gate) also governs pastes.
//
// Files land under FLEETDECK_HOME (a directory only the daemon's own user can
// write), NOT in shared os.tmpdir(). That is a security boundary, not a
// preference: an earlier cut wrote to a FIXED, world-known name in sticky /tmp,
// where any other local user could pre-plant a symlink there and turn our
// prune+write into an arbitrary-file delete/overwrite as the daemon's user
// (CWE-59/CWE-377). Under FLEETDECK_HOME no other user can create that entry in
// the first place, so the attack is structurally impossible; the lstat/uid
// guards below are belt-and-suspenders on top. The agent runs as the same user
// in the same box, so an absolute path here resolves for it.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { errCode } from './errors.ts';

// The image formats this ingest accepts — exactly Claude Code's image inputs.
// sniffImage returns one of these (from the bytes) or null.
type ImageExt = 'png' | 'jpg' | 'gif' | 'webp';

// The untrusted POST body. `data` is `unknown` because it arrives off the wire;
// pasteImage narrows it before use (the JS read `ev?.data` defensively too).
interface PasteEvent {
  data?: unknown;
}

// The {status, body} envelope — the same shape core.spawn returns. The
// discriminant `ok` keeps the error/success bodies apart without index access.
interface PasteErrBody {
  ok: false;
  reason: string;
}
interface PasteOkBody {
  ok: true;
  path: string;
  bytes: number;
}
interface PasteResult {
  status: number;
  body: PasteErrBody | PasteOkBody;
}

// Decoded-bytes cap. Distinct from the HTTP body cap (which sees base64, ~33%
// larger): this one is the contract on the IMAGE, that one is transport armor.
const MAX_IMAGE_BYTES = 10 * 1024 * 1024;

// Pastes older than this are pruned on the next paste. Not a retention.mjs
// branch on purpose — that sweep is DB-state-only, and these files' lifecycle
// (consumed at submit, worthless after) has nothing to do with fleet state.
const PRUNE_AFTER_MS = 24 * 60 * 60 * 1000;

// A hard ceiling regardless of age: age-only pruning cannot bound a burst
// inside the 24h window, and every paste is up to MAX_IMAGE_BYTES. Keep at most
// N (the most recent) so a flood (or a wedged agent that never submits) cannot
// grow the directory without limit. The count-prune runs AFTER each write, so N
// is the true on-disk count once a paste returns — not N+1 (see pasteImage).
const MAX_KEPT_PASTES = 50;

const ACCEPT = 'png, jpeg, gif, webp';

// FLEETDECK_HOME, resolved the same way fleetd.mjs does, at CALL time — tests
// (and any embedder) point each daemon at its own HOME via the env var.
function pasteDir(): string {
  // FLEETDECK_HOME falls through to the default when UNSET *or empty* — an
  // empty env var must never resolve the paste dir to `/pastes`. That
  // empty-string fallthrough is exactly what `||` does and `??` does NOT, so
  // this stays a truthiness test (a ternary, to satisfy prefer-nullish-
  // coalescing), never `??`. os.homedir()'s `|| os.tmpdir()` is the same story.
  const configured = process.env['FLEETDECK_HOME'];
  const home =
    configured != null && configured !== ''
      ? configured
      : path.join(os.homedir() || os.tmpdir(), '.fleetdeck');
  return path.join(home, 'pastes');
}

// Magic-byte sniff — the first binary ingest in the daemon, so the rule is
// strict: the CLIENT's mime claim is never trusted, the extension comes from
// the bytes, and bytes we do not recognize are refused outright. The accepted
// set is exactly what Claude Code accepts as image input. This gates INGEST; it
// is not a full decode — Claude does the real validation when it reads the file.
function sniffImage(buf: Buffer): ImageExt | null {
  if (
    buf.length >= 8 &&
    buf[0] === 0x89 &&
    buf[1] === 0x50 &&
    buf[2] === 0x4e &&
    buf[3] === 0x47 &&
    buf[4] === 0x0d &&
    buf[5] === 0x0a &&
    buf[6] === 0x1a &&
    buf[7] === 0x0a
  )
    return 'png';
  if (buf.length >= 3 && buf[0] === 0xff && buf[1] === 0xd8 && buf[2] === 0xff) return 'jpg';
  if (buf.length >= 6 && /^GIF8[79]a$/.test(buf.toString('latin1', 0, 6))) return 'gif';
  if (
    buf.length >= 12 &&
    buf.toString('latin1', 0, 4) === 'RIFF' &&
    buf.toString('latin1', 8, 12) === 'WEBP'
  )
    return 'webp';
  return null;
}

// Base64 is a well-formed alphabet; enforce it rather than leaning on
// Buffer.from's silence. Buffer.from(x,'base64') NEVER throws — it drops
// invalid characters — so without this check the "not base64" refusal is dead
// code and a malformed body decodes to whatever survived, then fails the sniff
// with a confusing error. Reject up front instead.
function looksBase64(s: string): boolean {
  const t = s.replace(/\s+/g, '');
  return t.length > 0 && t.length % 4 === 0 && /^[A-Za-z0-9+/]*={0,2}$/.test(t);
}

// Ensure the paste dir exists and is a real directory THIS user owns. Created
// non-recursively so a pre-existing symlink is not silently followed (mkdir on
// an existing path returns EEXIST for a symlink too); then lstat-verified.
function ensurePasteDir(): string {
  const dir = pasteDir();
  try {
    fs.mkdirSync(path.dirname(dir), { recursive: true });
  } catch {
    /* HOME may already exist */
  }
  try {
    fs.mkdirSync(dir, { mode: 0o700 });
  } catch (err) {
    if (errCode(err) !== 'EEXIST') throw err;
  }
  const st = fs.lstatSync(dir);
  if (st.isSymbolicLink() || !st.isDirectory()) {
    throw new Error(`${dir} is not a real directory; refusing to write pastes there`);
  }
  // Capture the accessor before the typeof guard so TS narrows the call site
  // (process.getuid is `(() => number) | undefined` off-Windows).
  const getuid = process.getuid;
  if (typeof getuid === 'function' && st.uid !== getuid()) {
    throw new Error(`${dir} is not owned by this user; refusing to write pastes there`);
  }
  return dir;
}

// One surviving regular file the count-prune weighs: its path and mtime.
interface PasteEntry {
  p: string;
  mtime: number;
}

// Age-then-count prune. Best-effort, errors swallowed: a prune must never fail a
// paste. lstat, not stat: a per-entry symlink is skipped, never followed to
// unlink something outside the dir.
// `protect` (an absolute path, the paste we just wrote) is NEVER pruned and is
// excluded from BOTH the age sweep and the count budget: the count-prune uses
// oldest-mtime-first, so a pre-existing file with a doctored FUTURE mtime would
// otherwise sort the fresh current-time paste as "oldest" and unlink the very
// file we are about to return. Reserving one slot for it keeps the on-disk total
// at MAX_KEPT_PASTES (no off-by-one) while guaranteeing the fresh paste survives.
function pruneOldPastes(
  dir: string,
  now: number = Date.now(),
  protect: string | null = null,
): void {
  let names: string[];
  try {
    names = fs.readdirSync(dir);
  } catch {
    return;
  }
  const regular: PasteEntry[] = [];
  for (const name of names) {
    const p = path.join(dir, name);
    if (protect && p === protect) continue; // the just-written paste is untouchable
    let st: fs.Stats;
    try {
      st = fs.lstatSync(p);
    } catch {
      continue;
    }
    if (!st.isFile()) continue; // skip symlinks, dirs, sockets — never chase them
    if (now - st.mtimeMs > PRUNE_AFTER_MS) {
      try {
        fs.unlinkSync(p);
      } catch {
        /* raced with another prune */
      }
    } else {
      regular.push({ p, mtime: st.mtimeMs });
    }
  }
  const budget = protect ? MAX_KEPT_PASTES - 1 : MAX_KEPT_PASTES; // reserve a slot
  if (regular.length > budget) {
    regular.sort((a, b) => a.mtime - b.mtime); // oldest first
    for (const { p } of regular.slice(0, regular.length - budget)) {
      try {
        fs.unlinkSync(p);
      } catch {
        /* raced */
      }
    }
  }
}

/**
 * POST /api/paste-image flow → {status, body}, the same envelope as
 * core.spawn. `data` is the image as base64 (raw base64 or a data: URL —
 * browsers produce the latter from FileReader.readAsDataURL, and stripping the
 * prefix here keeps the board dumb).
 */
export function pasteImage(ev: PasteEvent | null | undefined): PasteResult {
  const raw = ev != null && typeof ev.data === 'string' ? ev.data : '';
  // A data: URL only counts if it actually carries the base64 delimiter; a
  // 'data:' with no ',' is malformed, not "the whole string is base64".
  let b64 = raw;
  if (raw.startsWith('data:')) {
    const comma = raw.indexOf(',');
    if (comma === -1) return { status: 400, body: { ok: false, reason: 'malformed data URL' } };
    b64 = raw.slice(comma + 1);
  }
  if (!b64) return { status: 400, body: { ok: false, reason: 'missing image data' } };
  if (!looksBase64(b64)) return { status: 400, body: { ok: false, reason: 'not valid base64' } };

  const buf = Buffer.from(b64, 'base64');
  if (buf.length === 0) return { status: 400, body: { ok: false, reason: 'empty image' } };
  if (buf.length > MAX_IMAGE_BYTES) {
    return { status: 413, body: { ok: false, reason: `image exceeds ${MAX_IMAGE_BYTES} bytes` } };
  }

  const ext = sniffImage(buf);
  if (!ext)
    return { status: 400, body: { ok: false, reason: `not a supported image (${ACCEPT})` } };

  let dir: string;
  try {
    dir = ensurePasteDir();
  } catch (err) {
    console.error('fleetd paste: cannot prepare paste dir:', err);
    return { status: 500, body: { ok: false, reason: 'cannot prepare paste dir' } };
  }

  // Name from a 128-bit random id (not a per-second counter): collisions are
  // cryptographically negligible, so a burst can never overwrite an earlier
  // paste. No spaces or shell metacharacters — the path is TYPED, unquoted.
  const name = `paste-${crypto.randomUUID()}.${ext}`;
  const file = path.join(dir, name);
  const tmp = `${file}.tmp`;

  // write-temp (exclusive) + rename with owner-only perms: the path we hand back
  // must never point at a half-written file, and 'wx' refuses to clobber.
  try {
    fs.writeFileSync(tmp, buf, { mode: 0o600, flag: 'wx' });
    fs.renameSync(tmp, file);
  } catch (err) {
    try {
      fs.unlinkSync(tmp);
    } catch {
      /* nothing landed */
    }
    console.error('fleetd paste: write failed:', err);
    return { status: 500, body: { ok: false, reason: 'write failed' } };
  }

  // Prune AFTER the new file lands, so the cap holds POST-write. Pruning first
  // trimmed to MAX_KEPT_PASTES and then wrote → MAX_KEPT_PASTES+1: an off-by-one
  // that let the dir sit permanently one over (and ~one image over the byte
  // budget). Pass `file` as protected so the count-prune can never unlink the
  // paste we are about to return — even if a doctored future mtime on another
  // file would otherwise sort ours as "oldest". The 24h age-prune runs here too.
  // Both stay best-effort — a prune slip never unwinds a paste that already landed.
  pruneOldPastes(dir, Date.now(), file);

  return { status: 201, body: { ok: true, path: file, bytes: buf.length } };
}

// exported for tests only — the sniff table, dir resolution, and retention cap
// are contracts.
export { sniffImage, pasteDir, MAX_KEPT_PASTES };
