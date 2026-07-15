// tests/paste-image.test.mjs
//
// POST /api/paste-image — the server half of "Ctrl+V a screenshot into the
// board terminal" (paste.mjs). Pins the ingest contract:
//
//   - magic-byte sniff decides (png/jpg/gif/webp in, junk out) — the client's
//     mime claim is never consulted, the extension comes from the bytes
//   - the decoded-image cap 413s independently of the transport cap
//   - the per-route transport raise works: a >1 MB body (over the global
//     MAX_BODY) still lands on THIS path, and nowhere else
//   - written files are 0600, under the OS tmpdir, and atomic (no .tmp left)
//   - the CSRF walls hold: cross-site Origin → 403, non-JSON content-type → 415
//   - prune: a paste older than 24 h is reclaimed by the next paste, and the
//     retention cap holds at MAX_KEPT_PASTES POST-write (never N+1)
//
// Magic-byte stubs (not decodable images) are used on purpose: the endpoint's
// contract IS the sniff. Whether Claude can render the pixels is the caller's
// concern, exercised in live verification, not here.

import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postJson } from './helpers/http.mjs';
import { scaleMs } from './helpers/wait.mjs';

// The daemon writes under ITS FLEETDECK_HOME (startDaemon gives each a fresh
// tmpdir home), NOT a shared /tmp path — that move is the fix for the /tmp
// symlink-follow, so the test asserts the location relative to the daemon home.
const pasteDirOf = (d) => path.join(d.home, 'pastes');

const b64 = (buf) => Buffer.from(buf).toString('base64');

// Smallest buffers that satisfy the sniff. Padding past the header keeps them
// honest about "bytes after the magic don't matter to ingest".
const PNG = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), Buffer.alloc(64)]);
const JPG = Buffer.concat([Buffer.from([0xff, 0xd8, 0xff, 0xe0]), Buffer.alloc(64)]);
const GIF = Buffer.concat([Buffer.from('GIF89a', 'latin1'), Buffer.alloc(64)]);
const WEBP = Buffer.concat([Buffer.from('RIFF', 'latin1'), Buffer.alloc(4), Buffer.from('WEBP', 'latin1'), Buffer.alloc(64)]);

// fetch() refuses to set Origin, so the cross-site browser is simulated with a
// raw request — same technique as csrf-guard.test.mjs.
function rawPost(port, reqPath, headers, body) {
  return new Promise((resolve, reject) => {
    const req = http.request(
      { host: '127.0.0.1', port, path: reqPath, method: 'POST', headers },
      (res) => {
        let out = '';
        res.on('data', (d) => { out += d; });
        res.on('end', () => resolve({ status: res.statusCode, body: out }));
      },
    );
    req.setTimeout(scaleMs(5000), () => req.destroy(new Error('raw request timed out')));
    req.on('error', reject);
    req.end(body);
  });
}

test('paste-image: ingest contract', async (t) => {
  const d = await startDaemon();
  t.after(() => d.stop());
  const url = `${d.baseUrl}/api/paste-image`;
  const PASTE_DIR = pasteDirOf(d);

  await t.test('png/jpg/gif/webp are accepted and the extension comes from the sniff', async () => {
    for (const [buf, ext] of [[PNG, 'png'], [JPG, 'jpg'], [GIF, 'gif'], [WEBP, 'webp']]) {
      const res = await postJson(url, { data: b64(buf) });
      assert.equal(res.status, 201, `${ext}: ${JSON.stringify(res.json)}`);
      assert.equal(res.json.ok, true);
      assert.ok(res.json.path.endsWith(`.${ext}`), `${res.json.path} should end .${ext}`);
      assert.ok(res.json.path.startsWith(PASTE_DIR), `${res.json.path} must live under ${PASTE_DIR}`);
      assert.ok(fs.existsSync(res.json.path), 'file must exist');
      assert.equal(fs.statSync(res.json.path).mode & 0o777, 0o600, 'owner-only perms');
      assert.ok(!fs.existsSync(`${res.json.path}.tmp`), 'no staging file left behind');
    }
  });

  await t.test('the paste dir is owner-only and never in shared /tmp', async () => {
    await postJson(url, { data: b64(PNG) });
    const st = fs.lstatSync(PASTE_DIR);
    assert.ok(!st.isSymbolicLink(), 'paste dir must not be a symlink');
    assert.ok(st.isDirectory());
    assert.equal(st.mode & 0o777, 0o700, 'paste dir is owner-only');
    assert.ok(PASTE_DIR.startsWith(d.home), 'paste dir lives under FLEETDECK_HOME, not /tmp');
  });

  await t.test('a burst of pastes never collides (unique 128-bit names)', async () => {
    const results = await Promise.all(Array.from({ length: 40 }, () => postJson(url, { data: b64(PNG) })));
    const paths = new Set();
    for (const r of results) { assert.equal(r.status, 201); paths.add(r.json.path); }
    assert.equal(paths.size, results.length, 'every paste got a distinct path — no clobber');
  });

  await t.test('a data: URL prefix is stripped, not rejected', async () => {
    const res = await postJson(url, { data: `data:image/png;base64,${b64(PNG)}` });
    assert.equal(res.status, 201);
    assert.ok(res.json.path.endsWith('.png'));
  });

  await t.test('bytes that are not a supported image are refused', async () => {
    for (const junk of [b64(Buffer.from('hello, i am not an image')), b64(Buffer.alloc(32))]) {
      const res = await postJson(url, { data: junk });
      assert.equal(res.status, 400);
      assert.equal(res.json.ok, false);
    }
  });

  await t.test('malformed base64 is refused up front, not decoded to junk', async () => {
    for (const bad of ['!!!not base64!!!', 'iV!BO@Rw==', 'abc']) { // last: not a multiple of 4
      const res = await postJson(url, { data: bad });
      assert.equal(res.status, 400, `"${bad}" should 400`);
    }
  });

  await t.test('a data: URL with no comma is refused, not treated as base64', async () => {
    const res = await postJson(url, { data: 'data:image/png;base64NOCOMMA' });
    assert.equal(res.status, 400);
  });

  await t.test('missing/empty data is a 400, not a crash', async () => {
    for (const body of [{}, { data: '' }, { data: null }]) {
      const res = await postJson(url, body);
      assert.equal(res.status, 400);
    }
  });

  await t.test('the per-route transport raise works: a 2 MB image (over global MAX_BODY) lands', async () => {
    const big = Buffer.concat([PNG, Buffer.alloc(2 * 1024 * 1024)]);
    const res = await postJson(url, { data: b64(big) }, { timeout: scaleMs(15000) });
    assert.equal(res.status, 201, JSON.stringify(res.json));
    assert.equal(fs.statSync(res.json.path).size, big.length, 'decoded bytes written verbatim');
  });

  await t.test('decoded image over 10 MB is 413 even though transport allows it', async () => {
    const over = Buffer.concat([PNG, Buffer.alloc(10 * 1024 * 1024)]);
    const res = await postJson(url, { data: b64(over) }, { timeout: scaleMs(15000) });
    assert.equal(res.status, 413);
    assert.equal(res.json.ok, false);
  });

  await t.test('other POST paths keep the small global body cap', async () => {
    // ~1.5 MB to /command must still 413 — the raise is per-route, not global.
    const res = await postJson(`${d.baseUrl}/command`, { text: 'x'.repeat(1_500_000) });
    assert.equal(res.status, 413);
  });

  await t.test('cross-site Origin is refused before a byte of image is processed', async () => {
    const body = JSON.stringify({ data: b64(PNG) });
    const res = await rawPost(d.port, '/api/paste-image', {
      'content-type': 'application/json',
      'content-length': Buffer.byteLength(body),
      origin: 'https://evil.example.com',
    }, body);
    assert.equal(res.status, 403);
  });

  await t.test('non-JSON content-type is refused (the preflight-forcing wall)', async () => {
    const res = await rawPost(d.port, '/api/paste-image', {
      'content-type': 'application/octet-stream',
      'content-length': PNG.length,
    }, PNG);
    assert.equal(res.status, 415);
  });

  await t.test('a stale paste is pruned by the next paste', async () => {
    const res0 = await postJson(url, { data: b64(PNG) }); // ensures the dir exists, owned by us
    assert.equal(res0.status, 201);
    const old = path.join(PASTE_DIR, 'paste-00000000-0000-0000-0000-000000000000.png');
    fs.writeFileSync(old, PNG, { mode: 0o600 });
    const past = new Date(Date.now() - 25 * 60 * 60 * 1000);
    fs.utimesSync(old, past, past);
    const res = await postJson(url, { data: b64(PNG) });
    assert.equal(res.status, 201);
    assert.ok(!fs.existsSync(old), 'the 25h-old paste should have been reclaimed');
  });
});

// The security regression this release fixes, pinned directly against the
// module: a symlinked paste dir must be refused, never followed.
test('paste-image: a symlinked paste dir is refused (no /tmp symlink-follow)', async () => {
  const { pasteImage, pasteDir } = await import('../scripts/fleetd/paste.mjs');
  const { mkdtempSync, mkdirSync, symlinkSync, rmSync, existsSync } = await import('node:fs');
  const os2 = await import('node:os');
  const scratch = mkdtempSync(path.join(os2.tmpdir(), 'fd-paste-sym-'));
  const fakeHome = path.join(scratch, 'home');
  const victim = path.join(scratch, 'victim');
  mkdirSync(fakeHome, { recursive: true });
  mkdirSync(victim, { recursive: true });
  const prevHome = process.env.FLEETDECK_HOME;
  process.env.FLEETDECK_HOME = fakeHome;
  try {
    // plant a symlink where the paste dir would be created
    symlinkSync(victim, pasteDir());
    const res = pasteImage({ data: b64(PNG) });
    assert.equal(res.status, 500, 'must refuse a symlinked paste dir');
    assert.equal(existsSync(path.join(victim, 'x')), false); // nothing written into the target
    // and prove NOTHING landed in the victim dir
    const { readdirSync } = await import('node:fs');
    assert.deepEqual(readdirSync(victim), [], 'must not write into the symlink target');
  } finally {
    if (prevHome === undefined) delete process.env.FLEETDECK_HOME; else process.env.FLEETDECK_HOME = prevHome;
    rmSync(scratch, { recursive: true, force: true });
  }
});

test('paste-image: sniff table is a pinned contract', async () => {
  const { sniffImage } = await import('../scripts/fleetd/paste.mjs');
  assert.equal(sniffImage(PNG), 'png');
  assert.equal(sniffImage(JPG), 'jpg');
  assert.equal(sniffImage(GIF), 'gif');
  assert.equal(sniffImage(WEBP), 'webp');
  assert.equal(sniffImage(Buffer.from('RIFFxxxxWAVE', 'latin1')), null, 'RIFF alone is not WEBP');
  assert.equal(sniffImage(Buffer.alloc(2)), null, 'too short to sniff');
});

// Retention cap (MAX_KEPT_PASTES), pinned directly against the module — the
// export existed "for tests only" but nothing imported it, which is exactly how
// the prune off-by-one slipped through. Driven module-direct (no daemon) via
// FLEETDECK_HOME + pasteDir(), same harness as the symlink test above.
test('paste-image: an over-cap dir is pruned to exactly MAX_KEPT_PASTES, newest kept', async () => {
  const { pasteImage, pasteDir, MAX_KEPT_PASTES } = await import('../scripts/fleetd/paste.mjs');
  const os2 = await import('node:os');
  const scratch = fs.mkdtempSync(path.join(os2.tmpdir(), 'fd-paste-cap-'));
  const fakeHome = path.join(scratch, 'home');
  fs.mkdirSync(fakeHome, { recursive: true });
  const prevHome = process.env.FLEETDECK_HOME;
  process.env.FLEETDECK_HOME = fakeHome;
  try {
    const dir = pasteDir();
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
    // Seed MAX_KEPT_PASTES + 5 recent (within the 24h window) pastes with
    // strictly ascending mtimes, so "newest" is unambiguous and it is the
    // COUNT-prune, not the age-prune, that must do the trimming.
    const base = Date.now();
    const seeded = [];
    for (let i = 0; i < MAX_KEPT_PASTES + 5; i++) {
      const name = `paste-seed-${String(i).padStart(2, '0')}.png`;
      const p = path.join(dir, name);
      fs.writeFileSync(p, PNG, { mode: 0o600 });
      const t = new Date(base - (MAX_KEPT_PASTES + 5 - i) * 60_000); // i=0 oldest … last=newest
      fs.utimesSync(p, t, t);
      seeded.push(name);
    }
    assert.equal(fs.readdirSync(dir).length, MAX_KEPT_PASTES + 5, 'seeded over the cap');

    const res = pasteImage({ data: b64(PNG) });
    assert.equal(res.status, 201, JSON.stringify(res.body));

    const after = fs.readdirSync(dir);
    // 55 seeds + 1 fresh = 56 → prune the 6 oldest → exactly 50 remain.
    assert.equal(after.length, MAX_KEPT_PASTES, `exactly ${MAX_KEPT_PASTES} pastes must remain, got ${after.length}`);
    assert.ok(after.includes(path.basename(res.body.path)), 'the just-written paste (newest mtime) is retained');
    assert.ok(after.includes(seeded[seeded.length - 1]), 'the newest seed survives');
    assert.ok(!after.includes(seeded[0]), 'the oldest seed is pruned');
  } finally {
    if (prevHome === undefined) delete process.env.FLEETDECK_HOME; else process.env.FLEETDECK_HOME = prevHome;
    fs.rmSync(scratch, { recursive: true, force: true });
  }
});

// The off-by-one directly (bug #11): the prune once ran BEFORE the write, so a
// full dir (50 files → prune skips, not > 50 → write) settled permanently at
// 51. Assert the invariant holds after EVERY paste, and that the cap is
// actually reached (so a never-filled dir cannot pass this vacuously).
test('paste-image: repeated pastes never leave more than MAX_KEPT_PASTES on disk', async () => {
  const { pasteImage, pasteDir, MAX_KEPT_PASTES } = await import('../scripts/fleetd/paste.mjs');
  const os2 = await import('node:os');
  const scratch = fs.mkdtempSync(path.join(os2.tmpdir(), 'fd-paste-flood-'));
  const fakeHome = path.join(scratch, 'home');
  fs.mkdirSync(fakeHome, { recursive: true });
  const prevHome = process.env.FLEETDECK_HOME;
  process.env.FLEETDECK_HOME = fakeHome;
  try {
    const dir = pasteDir(); // pasteImage creates it; count what the cap governs
    let maxSeen = 0;
    for (let i = 0; i < MAX_KEPT_PASTES + 10; i++) {
      const res = pasteImage({ data: b64(PNG) });
      assert.equal(res.status, 201, JSON.stringify(res.body));
      const n = fs.readdirSync(dir).length;
      maxSeen = Math.max(maxSeen, n);
      assert.ok(n <= MAX_KEPT_PASTES, `after paste #${i + 1}, ${n} files on disk exceeds the cap ${MAX_KEPT_PASTES}`);
    }
    assert.equal(maxSeen, MAX_KEPT_PASTES, `the cap must actually be reached; saw a max of ${maxSeen}`);
  } finally {
    if (prevHome === undefined) delete process.env.FLEETDECK_HOME; else process.env.FLEETDECK_HOME = prevHome;
    fs.rmSync(scratch, { recursive: true, force: true });
  }
});
