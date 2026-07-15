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
// Files land in os.tmpdir()/fleetdeck-pastes, NOT the session's worktree and
// NOT FLEETDECK_HOME. The worktree would put the image in `git status` where
// an agent can absent-mindedly commit it; FLEETDECK_HOME persists (on Coder it
// is a PVC) and would grow forever. tmp is reclaimed by the OS/container, the
// agent reads the file the moment the prompt is submitted, and the prune below
// bounds the window in between.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';

// Decoded-bytes cap. Distinct from the HTTP body cap (which sees base64, ~33%
// larger): this one is the contract on the IMAGE, that one is transport armor.
const MAX_IMAGE_BYTES = 10 * 1024 * 1024;

// Pastes older than this are pruned on the next paste. Not a retention.mjs
// branch on purpose — that sweep is DB-state-only, and these files' lifecycle
// (consumed at submit, worthless after) has nothing to do with fleet state.
const PRUNE_AFTER_MS = 24 * 60 * 60 * 1000;

const PASTE_DIR = path.join(os.tmpdir(), 'fleetdeck-pastes');

// Magic-byte sniff — the first binary ingest in the daemon, so the rule is
// strict: the CLIENT's mime claim is never trusted, the extension comes from
// the bytes, and bytes we do not recognize are refused outright. The accepted
// set is exactly what Claude Code accepts as image input.
function sniffImage(buf) {
  if (buf.length >= 4 && buf[0] === 0x89 && buf[1] === 0x50 && buf[2] === 0x4e && buf[3] === 0x47) return 'png';
  if (buf.length >= 3 && buf[0] === 0xff && buf[1] === 0xd8 && buf[2] === 0xff) return 'jpg';
  if (buf.length >= 3 && buf[0] === 0x47 && buf[1] === 0x49 && buf[2] === 0x46) return 'gif';
  if (buf.length >= 12 && buf.toString('latin1', 0, 4) === 'RIFF' && buf.toString('latin1', 8, 12) === 'WEBP') return 'webp';
  return null;
}

// Best-effort, errors swallowed: a prune failure must never fail a paste.
function pruneOldPastes(now = Date.now()) {
  let names;
  try { names = fs.readdirSync(PASTE_DIR); } catch { return; }
  for (const name of names) {
    const p = path.join(PASTE_DIR, name);
    try {
      if (now - fs.statSync(p).mtimeMs > PRUNE_AFTER_MS) fs.unlinkSync(p);
    } catch { /* raced with another prune, or unreadable — either way skip */ }
  }
}

/**
 * POST /api/paste-image flow → {status, body}, the same envelope as
 * core.spawn. `data` is the image as base64 (raw base64 or a data: URL —
 * browsers produce the latter from FileReader.readAsDataURL, and stripping the
 * prefix here keeps the board dumb).
 */
export function pasteImage(ev) {
  const raw = typeof ev?.data === 'string' ? ev.data : '';
  const b64 = raw.startsWith('data:') ? raw.slice(raw.indexOf(',') + 1) : raw;
  if (!b64) return { status: 400, body: { ok: false, reason: 'missing image data' } };

  let buf;
  try { buf = Buffer.from(b64, 'base64'); } catch {
    return { status: 400, body: { ok: false, reason: 'not base64' } };
  }
  if (buf.length === 0) return { status: 400, body: { ok: false, reason: 'empty image' } };
  if (buf.length > MAX_IMAGE_BYTES) {
    return { status: 413, body: { ok: false, reason: `image exceeds ${MAX_IMAGE_BYTES} bytes` } };
  }

  const ext = sniffImage(buf);
  if (!ext) return { status: 400, body: { ok: false, reason: 'not a supported image (png, jpeg, gif, webp)' } };

  try { fs.mkdirSync(PASTE_DIR, { recursive: true, mode: 0o700 }); } catch (err) {
    console.error('fleetd paste: cannot create paste dir:', err);
    return { status: 500, body: { ok: false, reason: 'cannot create paste dir' } };
  }

  pruneOldPastes();

  // No spaces or shell metacharacters in the name — the whole point of the
  // file is to be TYPED into a terminal, unquoted.
  const ts = new Date().toISOString().replace(/[-:]/g, '').replace(/\..*$/, '');
  const name = `paste-${ts}-${crypto.randomBytes(3).toString('hex')}.${ext}`;
  const file = path.join(PASTE_DIR, name);
  const tmp = `${file}.tmp`;

  // write-temp + rename with owner-only perms, payload-capture house style: the
  // path we hand back must never point at a half-written file.
  try {
    fs.writeFileSync(tmp, buf, { mode: 0o600 });
    fs.renameSync(tmp, file);
  } catch (err) {
    try { fs.unlinkSync(tmp); } catch { /* nothing landed */ }
    console.error('fleetd paste: write failed:', err);
    return { status: 500, body: { ok: false, reason: 'write failed' } };
  }

  return { status: 201, body: { ok: true, path: file, bytes: buf.length } };
}

// exported for tests only — the sniff table is a contract worth pinning.
export { sniffImage, PASTE_DIR, MAX_IMAGE_BYTES };
