import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import http, { createServer } from 'node:http';
import fs, { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import type { AddressInfo } from 'node:net';
import { networkInterfaces, tmpdir } from 'node:os';
import path from 'node:path';
import { openDb } from '../src/daemon/db.ts';
import type { SqliteHandle } from '../src/daemon/sqlite.ts';
import { createFiles, LIST_MAX } from '../src/daemon/files.ts';
import { startDaemon } from './helpers/daemon.ts';
import { makePlainDir, makeRepoWithWorktree } from './helpers/gitrepo.ts';
import { getJson, postJson } from './helpers/http.ts';

interface FsEntry {
  name: string;
  type: string;
  size: number;
  mtime: number;
  ignored: boolean;
}
interface ListResponse {
  ok: boolean;
  path: string;
  git: boolean;
  entries: FsEntry[];
  truncated: boolean;
}
interface ReadResponse {
  binary: boolean;
  truncated: boolean;
  content: string;
  size: number;
}
interface SearchHit {
  path: string;
  line?: number;
  text?: string;
}
interface SearchResponse {
  ok: boolean;
  backend: string;
  hits: SearchHit[];
  truncated: boolean;
}
interface ReasonResponse {
  reason: string;
}
interface SettingsResponse {
  settings: { browse_root: { source: string; resolved: string } };
}

function withDb<T>(home: string, fn: (db: SqliteHandle) => T): T {
  const db = openDb(path.join(home, 'fleetd.db'));
  try {
    return fn(db);
  } finally {
    db.close();
  }
}

function seedSession(
  home: string,
  root: string,
  { sid = 'fs-session', spawnPath = null }: { sid?: string; spawnPath?: string | null } = {},
): void {
  withDb(home, (db) => {
    db.prepare(
      `INSERT INTO sessions
      (session_id, callsign, cwd, worktree, col, started_at, last_seen, source)
      VALUES (?, 'wren', ?, ?, 'idle', 1, 1, 'spawned')`,
    ).run(sid, root, root);
    if (spawnPath != null) {
      db.prepare(
        `INSERT INTO spawns
        (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
        VALUES (?, ?, 'wren', 'fleetdeck-test', 'fd-wren', ?, ?, 1, 'live')`,
      ).run(`spawn-${sid}`, sid, root, spawnPath);
    }
  });
}

function endpoint(baseUrl: string, sid: string, action: string, params = ''): string {
  return `${baseUrl}/api/sessions/${encodeURIComponent(sid)}/fs/${action}${params}`;
}

function raw(port: number, requestPath: string, headers: http.OutgoingHttpHeaders = {}) {
  return new Promise<{ status: number | undefined; body: unknown }>((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port, path: requestPath, headers }, (res) => {
      let body = '';
      res.on('data', (chunk: Buffer) => {
        body += chunk.toString();
      });
      res.on('end', () => {
        resolve({ status: res.statusCode, body: JSON.parse(body) as unknown });
      });
    });
    req.setTimeout(5000, () => req.destroy(new Error('request timed out')));
    req.on('error', reject);
    req.end();
  });
}

test('git session list/read/search is typed, literal, ignored-aware, and excludes .git', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-session-fs' });
  mkdirSync(path.join(repo.worktree, 'src'));
  writeFileSync(
    path.join(repo.worktree, 'src', 'alpha.txt'),
    'first line\nNeedle literal here\nthird line\n',
  );
  writeFileSync(path.join(repo.worktree, '.hidden'), 'dot file\n');
  writeFileSync(path.join(repo.worktree, '.gitignore'), '*.ignored\n');
  writeFileSync(path.join(repo.worktree, 'secret.ignored'), 'Needle must stay hidden\n');
  const daemon = await startDaemon();
  t.after(async () => {
    await daemon.stop();
    repo.cleanup();
  });
  seedSession(daemon.home, repo.root, { spawnPath: repo.worktree });

  const listed = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
  assert.equal(listed.status, 200);
  assert.equal((listed.json as ListResponse).git, true);
  assert.equal((listed.json as ListResponse).path, '');
  assert.equal(
    (listed.json as ListResponse).entries[0]?.name,
    'src',
    'directories sort before files',
  );
  assert.equal(
    (listed.json as ListResponse).entries.some((entry) => entry.name === '.git'),
    false,
  );
  assert.equal(
    (listed.json as ListResponse).entries.find((entry) => entry.name === 'src')?.type,
    'dir',
  );
  const ignored = (listed.json as ListResponse).entries.find(
    (entry) => entry.name === 'secret.ignored',
  );
  assert.ok(ignored, 'gitignored entry should be listed');
  assert.equal(ignored.ignored, true);
  assert.equal(typeof ignored.size, 'number');
  assert.equal(typeof ignored.mtime, 'number');

  const descended = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list', '?path=src'));
  assert.deepEqual(
    (descended.json as ListResponse).entries.map((entry) => [entry.name, entry.type]),
    [['alpha.txt', 'file']],
  );

  const read = await getJson(
    endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=src%2Falpha.txt'),
  );
  assert.equal(read.status, 200);
  assert.equal(
    (read.json as ReadResponse).content,
    'first line\nNeedle literal here\nthird line\n',
  );
  assert.equal((read.json as ReadResponse).binary, false);
  assert.equal((read.json as ReadResponse).truncated, false);
  const readDirectory = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read'));
  assert.equal(readDirectory.status, 404);
  assert.equal((readDirectory.json as ReasonResponse).reason, 'is a directory');

  const named = await getJson(
    endpoint(daemon.baseUrl, 'fs-session', 'search', '?mode=name&q=ALPHA'),
  );
  assert.equal(named.status, 200);
  assert.equal((named.json as SearchResponse).backend, 'git');
  assert.deepEqual((named.json as SearchResponse).hits, [{ path: 'src/alpha.txt' }]);

  const content = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', '?q=needle'));
  assert.equal(content.status, 200);
  assert.equal((content.json as SearchResponse).backend, 'git');
  assert.deepEqual((content.json as SearchResponse).hits, [
    { path: 'src/alpha.txt', line: 2, text: 'Needle literal here' },
  ]);
  assert.equal(
    (content.json as SearchResponse).hits.some((hit) => hit.path === 'secret.ignored'),
    false,
  );

  for (const q of ['-e', '((', 'a.*b']) {
    const adversarial = await getJson(
      endpoint(daemon.baseUrl, 'fs-session', 'search', `?q=${encodeURIComponent(q)}`),
    );
    assert.equal(adversarial.status, 200, `${q} is a literal query, not an option or regex`);
    assert.equal((adversarial.json as SearchResponse).backend, 'git');
    assert.deepEqual(
      (adversarial.json as SearchResponse).hits,
      [],
      `${q} matches nothing in the seeded tree`,
    );
  }

  writeFileSync(
    path.join(repo.worktree, 'adversarial.txt'),
    'aXXb regex-only cousin\na.*b literal needle\n(( literal parens\n-e literal dash e\n',
  );
  const literalHits = { '-e': 4, '((': 3, 'a.*b': 2 };
  for (const [q, line] of Object.entries(literalHits)) {
    const literal = await getJson(
      endpoint(daemon.baseUrl, 'fs-session', 'search', `?q=${encodeURIComponent(q)}`),
    );
    assert.equal(literal.status, 200, `${q} still resolves after the fixture lands`);
    assert.equal((literal.json as SearchResponse).backend, 'git');
    assert.equal(
      (literal.json as SearchResponse).hits.length,
      1,
      `${q} matches its literal line exactly once`,
    );
    assert.deepEqual((literal.json as SearchResponse).hits[0]?.path, 'adversarial.txt');
    assert.equal((literal.json as SearchResponse).hits[0]?.line, line);
  }
});

test('git content search stays plain with color.ui/color.grep always (BUG-027)', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-session-fs-color' });
  mkdirSync(path.join(repo.worktree, 'src'));
  writeFileSync(
    path.join(repo.worktree, 'src', 'alpha.txt'),
    'first line\nNeedle literal here\nthird line\n',
  );
  // The hostile operator config: grep wraps every path:line:text record in
  // ANSI escapes, which used to defeat parseGitGrep into a confident hits:[].
  for (const root of [repo.root, repo.worktree]) {
    execFileSync('git', ['config', 'color.ui', 'always'], { cwd: root });
    execFileSync('git', ['config', 'color.grep', 'always'], { cwd: root });
  }
  const daemon = await startDaemon();
  t.after(async () => {
    await daemon.stop();
    repo.cleanup();
  });
  seedSession(daemon.home, repo.root, { spawnPath: repo.worktree });

  const content = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', '?q=needle'));
  assert.equal(content.status, 200);
  assert.equal((content.json as SearchResponse).backend, 'git');
  assert.deepEqual((content.json as SearchResponse).hits, [
    { path: 'src/alpha.txt', line: 2, text: 'Needle literal here' },
  ]);
  assert.equal((content.json as SearchResponse).truncated, false);
});

test('traversal and symlink escapes never expose siblings, and walk search never follows links', async (t) => {
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
    '?path=../x',
    '?path=/etc/passwd',
    '?path=%2e%2e%2foutside.txt',
    '?path=a%00b',
  ]) {
    const response = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', params));
    assert.equal(response.status, 400, params);
  }
  const sibling = await getJson(
    endpoint(daemon.baseUrl, 'fs-session', 'read', `?path=..%2F${path.basename(outside)}`),
  );
  assert.equal(sibling.status, 400);

  const root = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
  assert.equal(
    (root.json as ListResponse).entries.find((entry) => entry.name === 'etc')?.type,
    'symlink',
  );
  const nested = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list', '?path=nested'));
  assert.equal(
    (nested.json as ListResponse).entries.find((entry) => entry.name === 'outside-link')?.type,
    'symlink',
  );
  assert.equal(
    (await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=etc%2Fpasswd'))).status,
    404,
  );
  assert.equal(
    (await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=nested%2Foutside-link')))
      .status,
    404,
  );
  const search = await getJson(
    endpoint(daemon.baseUrl, 'fs-session', 'search', '?q=outside-only-secret'),
  );
  assert.equal(search.status, 200);
  assert.equal((search.json as SearchResponse).backend, 'walk');
  assert.deepEqual((search.json as SearchResponse).hits, []);
});

test('plain roots include dotfiles, skip .git, and support list/read/walk search', async (t) => {
  const plain = makePlainDir();
  mkdirSync(path.join(plain.dir, '.git'));
  writeFileSync(path.join(plain.dir, '.git', 'hidden.txt'), 'never-index-this\n');
  writeFileSync(path.join(plain.dir, '.env'), 'DOT_VALUE=yes\n');
  writeFileSync(path.join(plain.dir, 'notes.txt'), 'zero\nplain needle\n');
  const daemon = await startDaemon();
  t.after(async () => {
    await daemon.stop();
    plain.cleanup();
  });
  seedSession(daemon.home, plain.dir);

  const list = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
  assert.equal((list.json as ListResponse).git, false);
  assert.equal(
    (list.json as ListResponse).entries.some((entry) => entry.name === '.env'),
    true,
  );
  assert.equal(
    (list.json as ListResponse).entries.some((entry) => entry.name === '.git'),
    false,
  );
  const read = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=.env'));
  assert.equal((read.json as ReadResponse).content, 'DOT_VALUE=yes\n');
  const content = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', '?q=plain'));
  assert.equal((content.json as SearchResponse).backend, 'walk');
  assert.deepEqual((content.json as SearchResponse).hits, [
    { path: 'notes.txt', line: 2, text: 'plain needle' },
  ]);
  const name = await getJson(
    endpoint(daemon.baseUrl, 'fs-session', 'search', '?mode=name&q=notes'),
  );
  assert.deepEqual((name.json as SearchResponse).hits, [{ path: 'notes.txt' }]);
  const skipped = await getJson(
    endpoint(daemon.baseUrl, 'fs-session', 'search', '?q=never-index-this'),
  );
  assert.deepEqual((skipped.json as SearchResponse).hits, []);

  // .git is refused for DIRECT access too, not merely hidden from listings and
  // search: reading .git/config on a plain clone would hand back embedded
  // remote credentials, and listing .git would hand back the object store.
  const gitList = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list', '?path=.git'));
  assert.equal(gitList.status, 404);
  const gitRead = await getJson(
    endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=.git%2Fhidden.txt'),
  );
  assert.equal(gitRead.status, 404);
});

test('read, list, search-hit, and binary caps shape bounded responses', async (t) => {
  const plain = makePlainDir();
  const exact = '123456789\n'.repeat(500);
  assert.equal(Buffer.byteLength(exact), 5000);
  writeFileSync(path.join(plain.dir, 'large.txt'), exact);
  writeFileSync(
    path.join(plain.dir, 'image.png'),
    Buffer.concat([
      Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00]),
      Buffer.from('binary payload'),
    ]),
  );
  for (let i = 0; i < 10; i += 1)
    writeFileSync(path.join(plain.dir, `hit-${i}.txt`), 'cap needle\n');
  const daemon = await startDaemon({
    env: {
      FLEETDECK_FS_READ_MAX: '1024',
      FLEETDECK_FS_LIST_MAX: '5',
      FLEETDECK_FS_SEARCH_HITS: '3',
    },
  });
  t.after(async () => {
    await daemon.stop();
    plain.cleanup();
  });
  seedSession(daemon.home, plain.dir);

  const read = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=large.txt'));
  assert.equal(read.status, 200);
  assert.equal((read.json as ReadResponse).size, 5000);
  assert.equal((read.json as ReadResponse).truncated, true);
  assert.ok((read.json as ReadResponse).content.length <= 1024);
  assert.ok(
    (read.json as ReadResponse).content === '' ||
      (read.json as ReadResponse).content.endsWith('\n'),
  );
  const list = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
  assert.equal((list.json as ListResponse).entries.length, 5);
  assert.equal((list.json as ListResponse).truncated, true);
  assert.equal(
    (list.json as ListResponse).entries.every((entry) => !entry.ignored),
    true,
  );
  const search = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'search', '?q=cap%20needle'));
  assert.equal((search.json as SearchResponse).hits.length, 3);
  assert.equal((search.json as SearchResponse).truncated, true);
  const binary = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=image.png'));
  assert.equal((binary.json as ReadResponse).binary, true);
  assert.equal(Object.hasOwn(binary.json as ReadResponse, 'content'), false);
});

test('list bounds lstat work, not just response size, in oversized directories (BUG-114)', async (t) => {
  const plain = makePlainDir();
  t.after(() => {
    plain.cleanup();
  });
  // Directories FIRST in locale order (aaa-*), then a swarm of files — the
  // worst case for a naive candidate window: it would lstat the whole swarm
  // before the leading dirs filled the page. LIST_MAX is a module constant
  // (default 1000), so total names = LIST_MAX dirs + LIST_MAX files.
  for (let i = 0; i < LIST_MAX; i += 1)
    mkdirSync(path.join(plain.dir, `aaa-${String(i).padStart(5, '0')}`));
  for (let i = 0; i < LIST_MAX; i += 1)
    writeFileSync(path.join(plain.dir, `zzz-${String(i).padStart(5, '0')}.txt`), 'x\n');

  let lstatCount = 0;
  const fsPatch = fs as { lstatSync: typeof fs.lstatSync };
  const realLstat = fsPatch.lstatSync;
  fsPatch.lstatSync = function counted(this: unknown, ...args: unknown[]): unknown {
    lstatCount += 1;
    return (realLstat as (...a: unknown[]) => unknown).apply(this, args);
  } as unknown as typeof fs.lstatSync;
  t.after(() => {
    fsPatch.lstatSync = realLstat;
  });

  const fakeCtx = {
    q: { getSession: { get: () => null }, spawnBySession: { get: () => null } },
    browseRootChoice: () => ({ source: 'override', resolved: plain.dir }),
  };
  const res = await createFiles(fakeCtx as unknown as Parameters<typeof createFiles>[0]).fsListHome(
    '',
  );
  assert.equal(res.status, 200);
  assert.equal(res.body['truncated'], true);
  assert.equal((res.body['entries'] as FsEntry[]).length, LIST_MAX);
  assert.equal(
    (res.body['entries'] as FsEntry[]).every((entry) => entry.type === 'dir'),
    true,
    'leading dirs still fill the page',
  );
  assert.ok(
    lstatCount <= LIST_MAX + 10,
    `lstat work must be bounded by LIST_MAX (${LIST_MAX}), not the directory size (${2 * LIST_MAX}); saw ${lstatCount}`,
  );
});

test('truncated git listings still mark gitignored entries in the returned page (BUG-115)', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-session-fs-trunc' });
  writeFileSync(path.join(repo.worktree, '.gitignore'), '*.ignored\n');
  writeFileSync(
    path.join(repo.worktree, 'aaa-secret.ignored'),
    'truncation must not hide ignore status\n',
  );
  for (let i = 0; i < 8; i += 1)
    writeFileSync(path.join(repo.worktree, `file-${i}.txt`), 'filler\n');
  const daemon = await startDaemon({ env: { FLEETDECK_FS_LIST_MAX: '5' } });
  t.after(async () => {
    await daemon.stop();
    repo.cleanup();
  });
  seedSession(daemon.home, repo.root, { spawnPath: repo.worktree });

  const listed = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
  assert.equal(listed.status, 200);
  assert.equal((listed.json as ListResponse).git, true);
  assert.equal((listed.json as ListResponse).truncated, true);
  assert.equal((listed.json as ListResponse).entries.length, 5);
  const ignored = (listed.json as ListResponse).entries.find(
    (entry) => entry.name === 'aaa-secret.ignored',
  );
  assert.ok(ignored, 'gitignored file sorts into the returned page');
  assert.equal(ignored.ignored, true);
});

test('truncated git listings still mark gitignored entries ignored:true (BUG-116)', async (t) => {
  // Regression: check-ignore used to run only when the listing was NOT
  // truncated, so a gitignored name inside the kept LIST_MAX slice came back
  // ignored:false while the same name was ignored:true in a full listing.
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-truncated-ignores' });
  writeFileSync(path.join(repo.worktree, '.gitignore'), 'mid.ignored\n');
  writeFileSync(path.join(repo.worktree, 'mid.ignored'), 'must stay annotated\n');
  for (const name of ['a1.txt', 'a2.txt', 'z1.txt', 'z2.txt', 'z3.txt']) {
    writeFileSync(path.join(repo.worktree, name), 'filler\n');
  }
  const daemon = await startDaemon({ env: { FLEETDECK_FS_LIST_MAX: '5' } });
  t.after(async () => {
    await daemon.stop();
    repo.cleanup();
  });
  seedSession(daemon.home, repo.root, { spawnPath: repo.worktree });

  const listed = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
  assert.equal(listed.status, 200);
  assert.equal((listed.json as ListResponse).truncated, true);
  assert.equal((listed.json as ListResponse).entries.length, 5);
  const ignored = (listed.json as ListResponse).entries.find(
    (entry) => entry.name === 'mid.ignored',
  );
  assert.ok(ignored, 'mid.ignored sorts into the kept slice');
  assert.equal(ignored.ignored, true);
});

test('unknown and removed roots report lifecycle status, and FIFO reads refuse promptly', async (t) => {
  const plain = makePlainDir();
  writeFileSync(path.join(plain.dir, 'file.txt'), 'present\n');
  let fifoAvailable = true;
  try {
    execFileSync('mkfifo', [path.join(plain.dir, 'pipe')]);
  } catch {
    fifoAvailable = false;
  }
  const daemon = await startDaemon();
  t.after(async () => {
    await daemon.stop();
    plain.cleanup();
  });
  seedSession(daemon.home, plain.dir);

  const unknown = await getJson(endpoint(daemon.baseUrl, 'does-not-exist', 'list'));
  assert.equal(unknown.status, 404);
  assert.equal((unknown.json as ReasonResponse).reason, 'unknown session');
  if (fifoAvailable) {
    const list = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'list'));
    assert.equal(
      (list.json as ListResponse).entries.find((entry) => entry.name === 'pipe')?.type,
      'other',
    );
    const started = Date.now();
    const read = await getJson(endpoint(daemon.baseUrl, 'fs-session', 'read', '?path=pipe'), {
      timeout: 1500,
    });
    assert.equal(read.status, 404);
    assert.ok(Date.now() - started < 1000, 'FIFO refusal must not block on open');
  }

  rmSync(plain.dir, { recursive: true, force: true });
  for (const action of ['list', 'read', 'search']) {
    const suffix = action === 'search' ? '?q=gone' : action === 'read' ? '?path=file.txt' : '';
    const response = await getJson(endpoint(daemon.baseUrl, 'fs-session', action, suffix));
    assert.equal(response.status, 410, action);
    assert.equal((response.json as ReasonResponse).reason, 'working tree no longer exists');
  }
});

async function reachableIpv4() {
  for (const entries of Object.values(networkInterfaces())) {
    for (const entry of entries ?? []) {
      if ((entry.family !== 'IPv4' && (entry.family as string | number) !== 4) || entry.internal)
        continue;
      const probe = createServer((_req, res) => res.end('ok'));
      try {
        await new Promise<void>((resolve, reject) => {
          probe.once('error', reject);
          probe.listen(0, entry.address, resolve);
        });
        const response = await fetch(
          `http://${entry.address}:${(probe.address() as AddressInfo).port}`,
          {
            signal: AbortSignal.timeout(750),
          },
        );
        if (response.ok) return entry.address;
      } catch {
        /* try another interface */
      } finally {
        await new Promise((resolve) => probe.close(resolve));
      }
    }
  }
  return null;
}

test('session filesystem routes stay behind LAN token and Host walls', async (t) => {
  const address = await reachableIpv4();
  if (!address) {
    t.skip('host has no reachable non-internal IPv4 interface');
    return;
  }
  const plain = makePlainDir();
  writeFileSync(path.join(plain.dir, 'visible.txt'), 'visible\n');
  const token = 'fleetdeck-session-fs-token-0123456789';
  const daemon = await startDaemon({ env: { FLEETDECK_BIND: '0.0.0.0', FLEETDECK_TOKEN: token } });
  t.after(async () => {
    await daemon.stop();
    plain.cleanup();
  });
  seedSession(daemon.home, plain.dir);
  const route = endpoint(`http://${address}:${daemon.port}`, 'fs-session', 'list');
  assert.equal((await fetch(route)).status, 401);
  assert.equal((await fetch(`${route}?t=${token}`)).status, 200);
  const hostile = await raw(daemon.port, '/api/sessions/fs-session/fs/list', {
    host: `evil.example:${daemon.port}`,
  });
  assert.equal(hostile.status, 403);
});

test('the global home explorer roots at HOME, browses and searches, and refuses escapes', async (t) => {
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
  assert.equal((list.json as ListResponse).ok, true);
  assert.equal(
    (list.json as ListResponse).entries.some((e) => e.name === 'workspace' && e.type === 'dir'),
    true,
  );
  assert.equal(
    (list.json as ListResponse).entries.some((e) => e.name === '.secret'),
    true,
  ); // dotfiles shown

  const read = await getJson(`${daemon.baseUrl}/api/fs/read?path=workspace%2Ftodo.md`);
  assert.equal((read.json as ReadResponse).content, 'find the home needle here\n');

  const search = await getJson(`${daemon.baseUrl}/api/fs/search?q=home%20needle&mode=content`);
  assert.equal((search.json as SearchResponse).ok, true);
  assert.deepEqual((search.json as SearchResponse).hits, [
    { path: 'workspace/todo.md', line: 1, text: 'find the home needle here' },
  ]);

  // containment: no traversal out of home, by relative or absolute path
  assert.equal(
    (
      await getJson(
        `${daemon.baseUrl}/api/fs/read?path=..%2F${path.basename(outside)}%2Fsecret.txt`,
      )
    ).status,
    400,
  );
  assert.equal(
    (
      await getJson(
        `${daemon.baseUrl}/api/fs/read?path=${encodeURIComponent(path.join(outside, 'secret.txt'))}`,
      )
    ).status,
    400,
  );
});

test('the global explorer re-roots to the browse_root setting, keeps containment, and 410s when it is deleted', async (t) => {
  const browse = mkdtempSync(path.join(tmpdir(), 'fleetdeck-browse-root-'));
  mkdirSync(path.join(browse, 'sub'));
  writeFileSync(path.join(browse, 'sub', 'inside.txt'), 'inside the configured root\n');
  const outside = mkdtempSync(path.join(tmpdir(), 'fleetdeck-browse-outside-'));
  writeFileSync(path.join(outside, 'secret.txt'), 'must never be reachable\n');
  // Clear any inherited Coder signal so precedence is deterministic (the setting
  // wins over everything regardless; this just documents intent).
  const daemon = await startDaemon({
    env: { CODER: '', CODER_WORKSPACE_NAME: '', CODER_AGENT_URL: '' },
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(browse, { recursive: true, force: true });
    rmSync(outside, { recursive: true, force: true });
  });

  const set = await postJson(`${daemon.baseUrl}/api/settings`, { browse_root: browse });
  assert.equal(set.status, 200, set.text);

  const list = await getJson(`${daemon.baseUrl}/api/fs/list?path=`);
  assert.equal(list.status, 200);
  assert.equal(
    (list.json as ListResponse).entries.some((e) => e.name === 'sub' && e.type === 'dir'),
    true,
  );
  const read = await getJson(`${daemon.baseUrl}/api/fs/read?path=sub%2Finside.txt`);
  assert.equal((read.json as ReadResponse).content, 'inside the configured root\n');

  // Containment survives the re-root — the resolver thunk changed, the walls did
  // not: no traversal out of the configured root, by relative or absolute path.
  assert.equal(
    (
      await getJson(
        `${daemon.baseUrl}/api/fs/read?path=..%2F${path.basename(outside)}%2Fsecret.txt`,
      )
    ).status,
    400,
  );
  assert.equal(
    (
      await getJson(
        `${daemon.baseUrl}/api/fs/read?path=${encodeURIComponent(path.join(outside, 'secret.txt'))}`,
      )
    ).status,
    400,
  );

  // Deleting the CONFIGURED root fails LOUD (410) NAMING the setting — never a
  // silent fall-through to home (which would leak a different tree).
  rmSync(browse, { recursive: true, force: true });
  const gone = await getJson(`${daemon.baseUrl}/api/fs/list?path=`);
  assert.equal(gone.status, 410);
  assert.match((gone.json as ReasonResponse).reason, /browse_root setting/i);
});

test('FLEETDECK_BROWSE_ROOT=/ is refused with a 410 naming the source, never served', async (t) => {
  // The settings validator bans a configured root of /, but the env var is
  // unvalidated — serving it would hand a LAN token-holder the entire
  // filesystem. The fs endpoints must refuse it loudly instead.
  const daemon = await startDaemon({
    env: {
      FLEETDECK_BROWSE_ROOT: '/',
      CODER: '',
      CODER_WORKSPACE_NAME: '',
      CODER_AGENT_URL: '',
    },
  });
  t.after(async () => {
    await daemon.stop();
  });
  for (const [action, suffix] of [
    ['list', '?path='],
    ['read', '?path=etc%2Fpasswd'],
    ['search', '?q=root'],
  ]) {
    const res = await getJson(`${daemon.baseUrl}/api/fs/${action}${suffix}`);
    assert.equal(res.status, 410, action);
    assert.match((res.json as ReasonResponse).reason, /FLEETDECK_BROWSE_ROOT/, action);
    assert.match((res.json as ReasonResponse).reason, /filesystem root/i, action);
  }
});

test('FLEETDECK_BROWSE_ROOT roots the global explorer, and the browse_root setting beats the env', async (t) => {
  const envRoot = mkdtempSync(path.join(tmpdir(), 'fleetdeck-browse-env-'));
  writeFileSync(path.join(envRoot, 'from-env.txt'), 'env root\n');
  const settingRoot = mkdtempSync(path.join(tmpdir(), 'fleetdeck-browse-setting-'));
  writeFileSync(path.join(settingRoot, 'from-setting.txt'), 'setting root\n');
  const daemon = await startDaemon({
    env: {
      FLEETDECK_BROWSE_ROOT: envRoot,
      CODER: '',
      CODER_WORKSPACE_NAME: '',
      CODER_AGENT_URL: '',
    },
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(envRoot, { recursive: true, force: true });
    rmSync(settingRoot, { recursive: true, force: true });
  });

  // env honored (no setting yet): rooted at envRoot, source 'env'.
  const envList = await getJson(`${daemon.baseUrl}/api/fs/list?path=`);
  assert.equal(envList.status, 200);
  assert.equal(
    (envList.json as ListResponse).entries.some((e) => e.name === 'from-env.txt'),
    true,
  );
  const envSettings = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal((envSettings.json as SettingsResponse).settings.browse_root.source, 'env');
  assert.equal((envSettings.json as SettingsResponse).settings.browse_root.resolved, envRoot);

  // setting beats env: after POST, the explorer roots at settingRoot.
  const set = await postJson(`${daemon.baseUrl}/api/settings`, { browse_root: settingRoot });
  assert.equal(set.status, 200, set.text);
  const settingList = await getJson(`${daemon.baseUrl}/api/fs/list?path=`);
  assert.equal(
    (settingList.json as ListResponse).entries.some((e) => e.name === 'from-setting.txt'),
    true,
  );
  assert.equal(
    (settingList.json as ListResponse).entries.some((e) => e.name === 'from-env.txt'),
    false,
  );
  const afterSettings = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal((afterSettings.json as SettingsResponse).settings.browse_root.source, 'override');
  assert.equal((afterSettings.json as SettingsResponse).settings.browse_root.resolved, settingRoot);
});

test('0.16.0 credential denylist: lexical, symlink, and search paths all refuse', async (t) => {
  const browse = mkdtempSync(path.join(tmpdir(), 'fleetdeck-deny-root-'));
  // Production default is HOME-rooted browsing, where ~/.ssh and
  // ~/.fleetdeck/token sit INSIDE the browse root — so every secret below
  // lives under `browse`. Plain containment would happily serve each of them;
  // every refusal here must come from the denylist / fleetHomeReal gates, or
  // the suite would stay green with those gates deleted (0.16.0 walls could
  // regress silently — the false-confidence layout this test used to have).
  mkdirSync(path.join(browse, '.ssh'));
  writeFileSync(path.join(browse, '.ssh', 'id_ed25519'), 'PRIVATE KEY MATERIAL\n');
  mkdirSync(path.join(browse, '.aws'));
  writeFileSync(path.join(browse, '.aws', 'credentials'), 'AWS SECRET MATERIAL\n');
  mkdirSync(path.join(browse, 'work'));
  writeFileSync(path.join(browse, 'work', 'notes.txt'), 'ordinary notes\n');
  // .docker is NOT a denied segment — only its config.json is denied, by path.
  mkdirSync(path.join(browse, '.docker'));
  writeFileSync(
    path.join(browse, '.docker', 'config.json'),
    '{"auths":{"registry":{"auth":"DOCKER REGISTRY SECRET"}}}\n',
  );
  writeFileSync(
    path.join(browse, '.docker', 'images.list'),
    'DOCKER REGISTRY SECRET listed harmlessly\n',
  );
  // The attack: a symlink inside the browse root pointing at an IN-ROOT
  // credential dir. realpathInside's containment check passes — only the
  // resolved-path segment denylist can refuse this.
  symlinkSync(path.join(browse, '.ssh'), path.join(browse, 'work', 'ssh-link'));
  // The daemon's home (holding its token) sits INSIDE the browse root too,
  // so only the fleetHomeReal gate keeps it unservable.
  const fleetHome = path.join(browse, 'fleet-home');

  const daemon = await startDaemon({
    home: fleetHome,
    env: { CODER: '', CODER_WORKSPACE_NAME: '', CODER_AGENT_URL: '' },
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(browse, { recursive: true, force: true });
  });
  const set = await postJson(`${daemon.baseUrl}/api/settings`, { browse_root: browse });
  assert.equal(set.status, 200, set.text);

  // Lexical: a direct path naming a denied segment 404s — and the targets
  // really exist under the root, so a missing lexical gate would 200, not 404.
  for (const p of [
    '.ssh%2Fid_ed25519',
    '.aws%2Fcredentials',
    '.gnupg',
    '.netrc',
    '.kube',
    '.docker%2Fconfig.json',
  ]) {
    const res = await getJson(`${daemon.baseUrl}/api/fs/read?path=${p}`);
    assert.equal(res.status, 404, `read ${p}`);
  }
  // Case-insensitive: HOME-rooted browsing on case-folding filesystems must
  // refuse the uppercase spelling too.
  assert.equal(
    (await getJson(`${daemon.baseUrl}/api/fs/read?path=.SSH%2Fid_ed25519`)).status,
    404,
    'read .SSH',
  );

  // Symlink: the requested path is clean; the RESOLVED path is denied.
  assert.equal(
    (await getJson(`${daemon.baseUrl}/api/fs/read?path=work%2Fssh-link%2Fid_ed25519`)).status,
    404,
    'symlinked read',
  );
  assert.equal(
    (await getJson(`${daemon.baseUrl}/api/fs/list?path=work%2Fssh-link`)).status,
    404,
    'symlinked list',
  );

  // Listings hide denied entries entirely (like .git), they do not render —
  // the in-root .ssh/.aws dirs must not appear next to ordinary entries.
  const rootList = await getJson(`${daemon.baseUrl}/api/fs/list?path=`);
  assert.equal(
    (rootList.json as ListResponse).entries.some((e) => e.name === 'work'),
    true,
    'work dir visible',
  );
  for (const denied of ['.ssh', '.aws']) {
    assert.equal(
      (rootList.json as ListResponse).entries.some((e) => e.name === denied),
      false,
      `${denied} hidden from listing`,
    );
  }
  // The daemon home dir itself may appear in a listing, but descending into
  // it must 404 via the fleetHomeReal gate — inside the root, containment
  // alone would list it.
  assert.equal(
    (await getJson(`${daemon.baseUrl}/api/fs/list?path=fleet-home`)).status,
    404,
    'fleet home list',
  );

  // Search must not read denied trees: with the material inside the root,
  // only the walker's deniedName filter keeps the private key text unfindable
  // (the backend is walk — browse is not a git repo).
  const search = await getJson(
    `${daemon.baseUrl}/api/fs/search?q=${encodeURIComponent('PRIVATE KEY MATERIAL')}`,
  );
  assert.deepEqual((search.json as SearchResponse).hits, [], 'search never returns denied content');

  // .docker/config.json is denied by PATH, not by segment: walk search must
  // skip it in both modes even though it walks into the .docker directory.
  const dockerContent = await getJson(
    `${daemon.baseUrl}/api/fs/search?q=${encodeURIComponent('DOCKER REGISTRY SECRET')}`,
  );
  assert.deepEqual(
    (dockerContent.json as SearchResponse).hits,
    [{ path: '.docker/images.list', line: 1, text: 'DOCKER REGISTRY SECRET listed harmlessly' }],
    'content search skips .docker/config.json',
  );
  const dockerName = await getJson(
    `${daemon.baseUrl}/api/fs/search?mode=name&q=${encodeURIComponent('config.json')}`,
  );
  assert.deepEqual(
    (dockerName.json as SearchResponse).hits,
    [],
    'name search hides .docker/config.json',
  );

  // FLEETDECK_HOME containment: the token lives at browse/fleet-home/token,
  // inside the root — a clean relative path reaches it unless fleetHomeReal
  // refuses it. The symlink chain must be refused the same way.
  assert.equal(
    (await getJson(`${daemon.baseUrl}/api/fs/read?path=fleet-home%2Ftoken`)).status,
    404,
    'fleet home token, direct path',
  );
  symlinkSync(daemon.home, path.join(browse, 'work', 'fleet-home-link'));
  assert.equal(
    (await getJson(`${daemon.baseUrl}/api/fs/read?path=work%2Ffleet-home-link%2Ftoken`)).status,
    404,
    'fleet home token',
  );
});

test('0.16.0 credential denylist: walk search never returns .docker/config.json', async (t) => {
  // The walk backend (browse is NOT a git repo) never runs validateRelPath per
  // entry, so the .docker/config.json special case — a denied FILE inside an
  // allowed dir — must be enforced by the walker itself. Read refuses this
  // exact path; content and name search must not ride around that wall.
  const browse = mkdtempSync(path.join(tmpdir(), 'fleetdeck-deny-docker-'));
  mkdirSync(path.join(browse, '.docker'));
  writeFileSync(
    path.join(browse, '.docker', 'config.json'),
    '{"auths":{"registry.example.com":{"auth":"DOCKERREGISTRYNEEDLE"}}}\n',
  );
  // Allowed neighbors prove the refusal is the config.json special case, not
  // a blanket .docker ban: other files under .docker stay listable/readable.
  writeFileSync(path.join(browse, '.docker', 'daemon.json'), '{ "DOCKERREGISTRYNEEDLE": true }\n');
  writeFileSync(path.join(browse, 'readme.txt'), 'DOCKERREGISTRYNEEDLE here too\n');

  const daemon = await startDaemon({
    env: { CODER: '', CODER_WORKSPACE_NAME: '', CODER_AGENT_URL: '' },
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(browse, { recursive: true, force: true });
  });
  const set = await postJson(`${daemon.baseUrl}/api/settings`, { browse_root: browse });
  assert.equal(set.status, 200, set.text);

  const content = await getJson(
    `${daemon.baseUrl}/api/fs/search?mode=content&q=DOCKERREGISTRYNEEDLE`,
  );
  assert.equal(content.status, 200, content.text);
  assert.equal(
    (content.json as SearchResponse).backend,
    'walk',
    'browse root is not a git repo, so the walk backend runs',
  );
  assert.equal(
    (content.json as SearchResponse).hits.some(
      (h) => h.path.toLowerCase().split('/').includes('.docker') && h.path.endsWith('config.json'),
    ),
    false,
    'content search never returns .docker/config.json',
  );
  assert.equal(
    (content.json as SearchResponse).hits.some((h) => h.path === 'readme.txt'),
    true,
    'ordinary file still matches',
  );
  assert.equal(
    (content.json as SearchResponse).hits.some((h) => h.path.split('/').pop() === 'daemon.json'),
    true,
    'other .docker/* files remain searchable',
  );

  const names = await getJson(`${daemon.baseUrl}/api/fs/search?mode=name&q=config.json`);
  assert.equal(names.status, 200, names.text);
  assert.deepEqual(
    (names.json as SearchResponse).hits,
    [],
    'name search never returns .docker/config.json',
  );

  // The special case is config.json ONLY: .docker itself stays visible and
  // its other entries stay listable and readable, as designed.
  const rootList = await getJson(`${daemon.baseUrl}/api/fs/list?path=`);
  assert.equal(
    (rootList.json as ListResponse).entries.some((e) => e.name === '.docker'),
    true,
    '.docker dir stays listable',
  );
  const dockerList = await getJson(`${daemon.baseUrl}/api/fs/list?path=.docker`);
  assert.equal(dockerList.status, 200, dockerList.text);
  assert.equal(
    (dockerList.json as ListResponse).entries.some((e) => e.name === 'daemon.json'),
    true,
    'daemon.json listed',
  );
  const neighbor = await getJson(`${daemon.baseUrl}/api/fs/read?path=.docker%2Fdaemon.json`);
  assert.equal(neighbor.status, 200, neighbor.text);
  assert.equal(
    (await getJson(`${daemon.baseUrl}/api/fs/read?path=.docker%2Fconfig.json`)).status,
    404,
    'read config.json',
  );
});
