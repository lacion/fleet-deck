import assert from 'node:assert/strict';
import { openDatabase, type SqliteHandle } from '../src/daemon/sqlite.ts';
import {
  bunPlatformTypeSurface,
  defineBunPlatformConformance,
  runBunPlatformConformance,
} from './helpers/bun-platform-conformance.ts';
import test from './helpers/harness-test.ts';

interface ConformanceRow {
  id: number;
  name: string;
}

test('Bun platform conformance: bun:sqlite satisfies the generic adapter contract', async () => {
  const suite = defineBunPlatformConformance<SqliteHandle>({
    adapter: 'fleetdeck/bun:sqlite',
    acquire: () => openDatabase(':memory:'),
    release: (database) => database.close(),
    cases: [
      {
        name: 'exec',
        run(database) {
          database.exec('CREATE TABLE item(id INTEGER PRIMARY KEY, name TEXT NOT NULL)');
        },
      },
      {
        name: 'prepare-run',
        run(database) {
          const result = database.prepare('INSERT INTO item(name) VALUES (?)').run('heron');
          assert.equal(result.changes, 1);
          assert.equal(typeof result.lastInsertRowid, 'number');
        },
      },
      {
        name: 'get-all-miss-normalization',
        run(database) {
          const statement = database.prepare<ConformanceRow>(
            'SELECT id, name FROM item WHERE id = ?',
          );
          assert.deepEqual(statement.get(1), { id: 1, name: 'heron' });
          assert.equal(statement.get(999), undefined);
          assert.deepEqual(
            database.prepare<ConformanceRow>('SELECT id, name FROM item ORDER BY id').all(),
            [{ id: 1, name: 'heron' }],
          );
        },
      },
    ],
  });

  const report = await runBunPlatformConformance(suite);
  assert.deepEqual(report, {
    adapter: 'fleetdeck/bun:sqlite',
    completedCases: ['exec', 'prepare-run', 'get-all-miss-normalization'],
    released: true,
  });
  assert.equal(typeof bunPlatformTypeSurface.file, 'function');
  assert.equal(typeof bunPlatformTypeSurface.serve, 'function');
  assert.equal(typeof bunPlatformTypeSurface.spawn, 'function');
  assert.equal(typeof bunPlatformTypeSurface.write, 'function');
});

test('Bun platform conformance: release runs once when a case fails', async () => {
  let releases = 0;
  const failure = new Error('adapter case failed');
  const suite = defineBunPlatformConformance({
    adapter: 'fixture',
    acquire: () => ({ open: true }),
    release(adapter) {
      adapter.open = false;
      releases++;
    },
    cases: [
      {
        name: 'failure',
        run() {
          throw failure;
        },
      },
    ],
  });

  await assert.rejects(runBunPlatformConformance(suite), failure);
  assert.equal(releases, 1);
});
