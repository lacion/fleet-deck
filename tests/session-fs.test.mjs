import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import http, { createServer } from 'node:http';
import {
  mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync,
} from 'node:fs';
import { networkInterfaces, tmpdir } from 'node:os';
import path from 'node:path';
import { openDb } from '../scripts/fleetd/db.mjs';
import { startDaemon } from './helpers/daemon.mjs';
import { makePlainDir, makeRepoWithWorktree } from './helpers/gitrepo.mjs';
import { getJson } from './helpers/http.mjs';

function withDb(home, fn) {
  const db = openDb(path.join(home, 'fleetd.db'));
  try { return fn(db); } finally { db.close(); }
}

function seedSession(home, root, { sid = 'fs-session', spawnPath = null } = {}) {
  withDb(home, db => {
    db.prepare(`INSERT INTO sessions
      (session_id, callsign, cwd, worktree, col, started_at, last_seen, source)
      VALUES (?, 'wren', ?, ?, 'idle', 1, 1, 'spawned')`).run(sid, root, root);
    if (spawnPath != null) {
      db.prepare(`INSERT INTO spawns
        (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
        VALUES (?, ?, 'wren', 'fleetdeck-test', 'fd-wren', ?, ?, 1, 'live')`)
        .run(`spawn-${sid}`, sid, root, spawnPath);
    }
  });
}

function endpoint(baseUrl, sid, action, params = '') {
  return `${baseUrl}/api/sessions/${encodeURIComponent(sid)}/fs/${action}${params}`;
}

function raw(port, requestPath, headers = {}) {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port, path: requestPath, headers }, res => {
      let body = '';
      res.on('data', chunk => { body += chunk; });
      res.on('end', () => resolve({ status: res.statusCode, body: JSON.parse(body) }));
    });
    req.setTimeout(5000, () => req.destroy(new Error('request timed out')));
    req.on('error', reject);
    req.end();
  });
}

test('git session list/read/search is typed, literal, ignored-aware, and excludes .git', async t => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-session-fs' });
  mkdirSync(path.join(repo.worktree, 'src'));
  writeFileSync(path.join(repo.worktree, 'src', 'alpha.txt'), 'first line\nNeedle literal here\nthird line\n');
  writeFileSync(path.join(repo.worktree, '.hidden'), 'dot file\n');
  writeFileSync(path.join(repo.worktree, '.gitignore'), '*.ignored\n');
  writeFileSync(path.join(repo.worktree, 'secret.ignored'), 'Needle must stay hidden\n');
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); repo.cleanup(); });
  seedSession(daemon.home, repo.root, { spawnPath: repo.worktree });

  const listed = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
  assert.equal(listed.status, 200);
  assert.equal(listed.json.git, true);
  assert.equal(listed.json.path, '');
  assert.equal(listed.json.entries[0].name, 'src', 'directories sort before files');
  assert.equal(listed.json.entries.some(entry => entry.name === '.git'), false);
  assert.equal(listed.json.entries.find(entry => entry.name === 'src').type, 'dir');
  const ignored = listed.json.entries.find(entry => entry.name === 'secret.ignored');
  assert.equal(ignored.ignored, true);
  assert.equal(typeof ignored.size, 'number');
  assert.equal(typeof ignored.mtime, 'number');

  const descended = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list', '?path=src'));
  assert.deepEqual(descended.json.entries.map(entry => [entry.name, entry.type]), [['alpha.txt', 'file']]);

  const read = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=src%2Falpha.txt'));
  assert.equal(read.status, 200);
  assert.equal(read.json.content, 'first line\nNeedle literal here\nthird line\n');
  assert.equal(read.json.binary, false);
  assert.equal(read.json.truncated, false);
  const readDirectory = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read'));
  assert.equal(readDirectory.status, 404);
  assert.equal(readDirectory.json.reason, 'is a directory');

  const named = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', '?mode=name&q=ALPHA'));
  assert.equal(named.status, 200);
  assert.equal(named.json.backend, 'git');
  assert.deepEqual(named.json.hits, [{ path: 'src/alpha.txt' }]);

  const content = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', '?q=needle'));
  assert.equal(content.status, 200);
  assert.equal(content.json.backend, 'git');
  assert.deepEqual(content.json.hits, [{ path: 'src/alpha.txt', line: 2, text: 'Needle literal here' }]);
  assert.equal(content.json.hits.some(hit => hit.path === 'secret.ignored'), false);

  for (const q of ['-e', '((', 'a.*b']) {
    const adversarial = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', `?q=${encodeURIComponent(q)}`));
    assert.equal(adversarial.status, 200, `${q} is a literal query, not an option or regex`);
  }
});

test('traversal and symlink escapes never expose siblings, and walk search never follows links', async t => {
  const plain = makePlainDir();
  const outside = path.join(path.dirname(plain.dir), `outside-${path.basename(plain.dir)}.txt`);
  writeFileSync(outside, 'outside-only-secret\n');
  mkdirSync(path.join(plain.dir, 'nested'));
  symlinkSync('/etc', path.join(plain.dir, 'etc'));
  symlinkSync(`../../${path.basename(outside)}`, path.join(plain.dir, 'nested', 'outside-link'));
  const daemon = await startDaemon();
  t.after(async () => {
    await daemon.stop();
    plain.cleanup();
    rmSync(outside, { force: true });
  });
  seedSession(daemon.home, plain.dir);

  for (const params of [
    '?path=../x', '?path=/etc/passwd', '?path=%2e%2e%2foutside.txt', '?path=a%00b',
  ]) {
    const response = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', params));
    assert.equal(response.status, 400, params);
  }
  const sibling = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', `?path=..%2F${path.basename(outside)}`));
  assert.equal(sibling.status, 400);

  const root = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
  assert.equal(root.json.entries.find(entry => entry.name === 'etc').type, 'symlink');
  const nested = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list', '?path=nested'));
  assert.equal(nested.json.entries.find(entry => entry.name === 'outside-link').type, 'symlink');
  assert.equal((await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=etc%2Fpasswd'))).status, 404);
  assert.equal((await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=nested%2Foutside-link'))).status, 404);
  const search = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', '?q=outside-only-secret'));
  assert.equal(search.status, 200);
  assert.equal(search.json.backend, 'walk');
  assert.deepEqual(search.json.hits, []);
});

test('plain roots include dotfiles, skip .git, and support list/read/walk search', async t => {
  const plain = makePlainDir();
  mkdirSync(path.join(plain.dir, '.git'));
  writeFileSync(path.join(plain.dir, '.git', 'hidden.txt'), 'never-index-this\n');
  writeFileSync(path.join(plain.dir, '.env'), 'DOT_VALUE=yes\n');
  writeFileSync(path.join(plain.dir, 'notes.txt'), 'zero\nplain needle\n');
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); plain.cleanup(); });
  seedSession(daemon.home, plain.dir);

  const list = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
  assert.equal(list.json.git, false);
  assert.equal(list.json.entries.some(entry => entry.name === '.env'), true);
  assert.equal(list.json.entries.some(entry => entry.name === '.git'), false);
  const read = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=.env'));
  assert.equal(read.json.content, 'DOT_VALUE=yes\n');
  const content = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', '?q=plain'));
  assert.equal(content.json.backend, 'walk');
  assert.deepEqual(content.json.hits, [{ path: 'notes.txt', line: 2, text: 'plain needle' }]);
  const name = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', '?mode=name&q=notes'));
  assert.deepEqual(name.json.hits, [{ path: 'notes.txt' }]);
  const skipped = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', '?q=never-index-this'));
  assert.deepEqual(skipped.json.hits, []);

  // .git is refused for DIRECT access too, not merely hidden from listings and
  // search: reading .git/config on a plain clone would hand back embedded
  // remote credentials, and listing .git would hand back the object store.
  const gitList = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list', '?path=.git'));
  assert.equal(gitList.status, 404);
  const gitRead = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=.git%2Fhidden.txt'));
  assert.equal(gitRead.status, 404);
});

test('read, list, search-hit, and binary caps shape bounded responses', async t => {
  const plain = makePlainDir();
  const exact = '123456789\n'.repeat(500);
  assert.equal(Buffer.byteLength(exact), 5000);
  writeFileSync(path.join(plain.dir, 'large.txt'), exact);
  writeFileSync(path.join(plain.dir, 'image.png'), Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00]),
    Buffer.from('binary payload'),
  ]));
  for (let i = 0; i < 10; i += 1) writeFileSync(path.join(plain.dir, `hit-${i}.txt`), 'cap needle\n');
  const daemon = await startDaemon({ env: {
    FLEETDECK_FS_READ_MAX: '1024',
    FLEETDECK_FS_LIST_MAX: '5',
    FLEETDECK_FS_SEARCH_HITS: '3',
  } });
  t.after(async () => { await daemon.stop(); plain.cleanup(); });
  seedSession(daemon.home, plain.dir);

  const read = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=large.txt'));
  assert.equal(read.status, 200);
  assert.equal(read.json.size, 5000);
  assert.equal(read.json.truncated, true);
  assert.ok(read.json.content.length <= 1024);
  assert.ok(read.json.content === '' || read.json.content.endsWith('\n'));
  const list = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
  assert.equal(list.json.entries.length, 5);
  assert.equal(list.json.truncated, true);
  assert.equal(list.json.entries.every(entry => entry.ignored === false), true);
  const search = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', '?q=cap%20needle'));
  assert.equal(search.json.hits.length, 3);
  assert.equal(search.json.truncated, true);
  const binary = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=image.png'));
  assert.equal(binary.json.binary, true);
  assert.equal(Object.hasOwn(binary.json, 'content'), false);
});

test('unknown and removed roots report lifecycle status, and FIFO reads refuse promptly', async t => {
  const plain = makePlainDir();
  writeFileSync(path.join(plain.dir, 'file.txt'), 'present\n');
  let fifoAvailable = true;
  try { execFileSync('mkfifo', [path.join(plain.dir, 'pipe')]); } catch { fifoAvailable = false; }
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); plain.cleanup(); });
  seedSession(daemon.home, plain.dir);

  const unknown = await getJson(endpoint(daemon.baseUrl, 'does-not-exist', 'list'));
  assert.equal(unknown.status, 404);
  assert.equal(unknown.json.reason, 'unknown session');
  if (fifoAvailable) {
    const list = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
    assert.equal(list.json.entries.find(entry => entry.name === 'pipe').type, 'other');
    const started = Date.now();
    const read = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=pipe'), { timeout: 1500 });
    assert.equal(read.status, 404);
    assert.ok(Date.now() - started < 1000, 'FIFO refusal must not block on open');
  }

  rmSync(plain.dir, { recursive: true, force: true });
  for (const action of ['list', 'read', 'search']) {
    const suffix = action === 'search' ? '?q=gone' : action === 'read' ? '?path=file.txt' : '';
    const response = await getJson(endpoint(daemon.baseUrl, 'fs-session', action, suffix));
    assert.equal(response.status, 410, action);
    assert.equal(response.json.reason, 'working tree no longer exists');
  }
});

async function reachableIpv4() {
  for (const entries of Object.values(networkInterfaces())) {
    for (const entry of entries || []) {
      if ((entry.family !== 'IPv4' && entry.family !== 4) || entry.internal) continue;
      const probe = createServer((_req, res) => res.end('ok'));
      try {
        await new Promise((resolve, reject) => {
          probe.once('error', reject);
          probe.listen(0, entry.address, resolve);
        });
        const response = await fetch(`http://${entry.address}:${probe.address().port}`, {
          signal: AbortSignal.timeout(750),
        });
        if (response.ok) return entry.address;
      } catch { /* try another interface */ } finally {
        await new Promise(resolve => probe.close(resolve));
      }
    }
  }
  return null;
}

test('session filesystem routes stay behind LAN token and Host walls', async t => {
  const address = await reachableIpv4();
  if (!address) return t.skip('host has no reachable non-internal IPv4 interface');
  const plain = makePlainDir();
  writeFileSync(path.join(plain.dir, 'visible.txt'), 'visible\n');
  const token = 'fleetdeck-session-fs-token-0123456789';
  const daemon = await startDaemon({ env: { FLEETDECK_BIND: '0.0.0.0', FLEETDECK_TOKEN: token } });
  t.after(async () => { await daemon.stop(); plain.cleanup(); });
  seedSession(daemon.home, plain.dir);
  const route = endpoint(`http://${address}:${daemon.port}`, 'fs-session', 'list');
  assert.equal((await fetch(route)).status, 401);
  assert.equal((await fetch(`${route}?t=${token}`)).status, 200);
  const hostile = await raw(daemon.port, '/api/sessions/fs-session/fs/list', { host: `evil.example:${daemon.port}` });
  assert.equal(hostile.status, 403);
});

test('the global home explorer roots at HOME, browses and searches, and refuses escapes', async t => {
  // A controlled home: os.homedir() honours HOME, so the daemon roots the
  // /api/fs/* endpoints here and the assertions are deterministic.
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-home-'));
  mkdirSync(path.join(home, 'workspace'));
  writeFileSync(path.join(home, 'notes.txt'), 'top-level home note\n');
  writeFileSync(path.join(home, 'workspace', 'todo.md'), 'find the home needle here\n');
  writeFileSync(path.join(home, '.secret'), 'a dotfile is shown, not hidden\n');
  const outside = mkdtempSync(path.join(tmpdir(), 'fleetdeck-outside-'));
  writeFileSync(path.join(outside, 'secret.txt'), 'must never be reachable\n');
  const daemon = await startDaemon({ env: { HOME: home } });
  t.after(async () => {
    await daemon.stop();
    rmSync(home, { recursive: true, force: true });
    rmSync(outside, { recursive: true, force: true });
  });

  const list = await getJson(`${daemon.baseUrl}/api/fs/list?path=`);
  assert.equal(list.status, 200);
  assert.equal(list.json.ok, true);
  assert.equal(list.json.entries.some(e => e.name === 'workspace' && e.type === 'dir'), true);
  assert.equal(list.json.entries.some(e => e.name === '.secret'), true); // dotfiles shown

  const read = await getJson(`${daemon.baseUrl}/api/fs/read?path=workspace%2Ftodo.md`);
  assert.equal(read.json.content, 'find the home needle here\n');

  const search = await getJson(`${daemon.baseUrl}/api/fs/search?q=home%20needle&mode=content`);
  assert.equal(search.json.ok, true);
  assert.deepEqual(search.json.hits, [{ path: 'workspace/todo.md', line: 1, text: 'find the home needle here' }]);

  // containment: no traversal out of home, by relative or absolute path
  assert.equal((await getJson(`${daemon.baseUrl}/api/fs/read?path=..%2F${path.basename(outside)}%2Fsecret.txt`)).status, 400);
  assert.equal((await getJson(`${daemon.baseUrl}/api/fs/read?path=${encodeURIComponent(path.join(outside, 'secret.txt'))}`)).status, 400);
});
