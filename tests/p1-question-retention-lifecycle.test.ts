import assert from 'node:assert/strict';
import test from './helpers/harness-test.ts';
import { openDb } from '../src/daemon/db.ts';
import { createCore } from '../src/daemon/derive.ts';
import { createQuestions } from '../src/daemon/questions.ts';

type CoreTmuxAdapter = NonNullable<NonNullable<Parameters<typeof createCore>[1]>['tmuxAdapter']>;

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
} {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

test('P1 questions lifecycle quiesces intake, releases held hooks as 200 {}, and closes once', async () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 60_000, sweepMs: 10 });
  const released: { body: unknown; status: number | undefined }[] = [];

  const held = questions.create('permission', 'held', {
    tool_name: 'Bash',
    tool_input: { command: 'true' },
  });
  questions.attachHold(held, (body, status) => released.push({ body, status }));

  // Answering a sibling arms the completed-key TTL timer; close owns that
  // timer too, while the first hold remains available for release proof.
  const completed = questions.create('permission', 'completed', {
    tool_name: 'Bash',
    tool_input: { command: 'printf done' },
  });
  questions.attachHold(completed, () => {
    /* the board answer below owns this response */
  });
  assert.equal(questions.answer(completed.id, { behavior: 'allow' }).status, 200);
  const pending = questions.create('freeform', 'pending', { text: 'Continue?' });

  assert.equal(questions.quiesce(), true);
  assert.equal(questions.quiesce(), false, 'quiesce is idempotent');
  assert.throws(
    () => questions.create('freeform', 'late', { text: 'Too late?' }),
    /questions are quiescing/,
  );
  assert.equal(
    questions.answer(pending.id, { text: 'yes' }).status,
    503,
    'post-quiesce mutation is refused',
  );

  const closeA = questions.close();
  const closeB = questions.close();
  assert.strictEqual(closeB, closeA, 'double close shares one settle promise');
  await closeA;
  assert.strictEqual(questions.close(), closeA, 'closed lifecycle keeps the same promise');
  assert.deepEqual(released, [{ body: {}, status: 200 }]);
  assert.equal(questions.isHeld(held.id), false);
  assert.deepEqual(
    questions.pendingOf('pending').map((row) => row.id),
    [pending.id],
    'quiesce refused the answer instead of mutating its row',
  );

  db.close();
});

test('P1 questions close cancels hold, rearm, and orphan callbacks before DB close', async () => {
  const db = openDb(':memory:');
  let callbacks = 0;
  const questions = createQuestions(db, {
    holdMs: 15,
    rearmGraceMs: 60,
    sweepMs: 10,
    tick: () => {
      callbacks++;
    },
    onChange: () => {
      callbacks++;
    },
    onRetired: () => {
      callbacks++;
    },
  });

  const row = questions.create('permission', 'timer', {
    tool_name: 'Bash',
    tool_input: { command: 'true' },
  });
  let settleReply!: (value: unknown) => void;
  const reply = new Promise<unknown>((resolve) => {
    settleReply = resolve;
  });
  questions.attachHold(row, settleReply);
  assert.deepEqual(
    await Promise.race([
      reply,
      delay(250).then(() => {
        throw new Error('hold expiry did not settle');
      }),
    ]),
    {},
  );

  // The expired hold has a rearm timer; this unattached row gives the orphan
  // cadence work it would perform if close failed to stop it.
  const orphan = questions.create('permission', 'orphan', { tool_name: 'Bash' });
  await questions.close();
  const atClose = callbacks;
  await delay(90);
  assert.equal(callbacks, atClose, 'no timer callback reaches user mutation hooks after close');
  assert.deepEqual(
    questions.pendingOf('timer'),
    [],
    'the cancelled rearm timer did not raise a successor',
  );
  assert.deepEqual(
    questions.pendingOf('orphan').map((candidate) => candidate.id),
    [orphan.id],
    'the cancelled orphan sweep did not mutate durable state',
  );

  db.close();
  await delay(30);
  assert.equal(callbacks, atClose, 'no owned callback runs against the closed database');
});

test('P5 createCore exposes retention work without starting or owning its schedule', async () => {
  const db = openDb(':memory:');
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions
      (session_id, callsign, col, started_at, last_seen, source)
     VALUES (?, ?, 'idle', ?, ?, 'hooks')`,
  ).run('retained', 'otter', now - 20_000_000, 0);
  db.prepare(
    `INSERT INTO spawns
      (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
     VALUES (?, ?, ?, ?, ?, ?, 'live')`,
  ).run('spawn-retained', 'retained', 'otter', 'fleetdeck-4711', 'fd4711-otter', now);

  const windows = deferred<[]>();
  let listCalls = 0;
  const adapter = {
    spawnOverrideCmd: () => null,
    listScopedWindows: () => {
      listCalls++;
      return windows.promise;
    },
    paneCurrentCommand: () => Promise.resolve(null),
    killWindowVerified: () => Promise.resolve({ ok: true }),
    hasTmux: () => true,
    capturePane: () => Promise.resolve(''),
    pasteText: () => Promise.resolve(true),
    sendEnter: () => Promise.resolve(true),
    sendBringupEnter: () => Promise.resolve(true),
    launchOverride: () => {
      /* unused */
    },
    ensureSession: () => Promise.resolve('fleetdeck-4711'),
    newWindow: () => Promise.reject(new Error('unused')),
    sessionName: () => 'fleetdeck-4711',
    windowName: (_port: number, callsign: string) => `fd4711-${callsign}`,
    typeAndEnter: () => Promise.resolve(true),
  } as unknown as CoreTmuxAdapter;
  const core = createCore(db, { port: 4711, home: '/p1-retention', tmuxAdapter: adapter });
  assert.equal(listCalls, 0, 'createCore did not launch a legacy boot retention sweep');

  db.prepare(
    'INSERT INTO events (session_id, hook_event, tool_name, note, at) VALUES (?, ?, ?, ?, ?)',
  ).run('retained', 'old', null, null, now - 20);
  db.prepare(
    'INSERT INTO events (session_id, hook_event, tool_name, note, at) VALUES (?, ?, ?, ?, ?)',
  ).run('retained', 'new', null, null, now);
  core.pruneEvents(now - 10);
  assert.deepEqual(
    db.prepare<{ hook_event: string }>('SELECT hook_event FROM events ORDER BY id').all(),
    [{ hook_event: 'new' }],
    'the Effect schedule can drive the public event-prune capability',
  );

  const sweep = core.retentionSweep(now);
  assert.equal(listCalls, 1, 'the ownerless callback starts only when its caller runs it');
  let sweepSettled = false;
  void sweep.then(() => {
    sweepSettled = true;
  });

  const closeA = core.lifecycle.close();
  const closeB = core.lifecycle.close();
  assert.strictEqual(closeB, closeA, 'combined core close is idempotent');
  await closeA;
  assert.equal(
    sweepSettled,
    false,
    'core lifecycle does not compete with the Effect owner for an admitted sweep',
  );

  windows.resolve([]);
  await sweep;
  await core.retentionSweep(now);
  assert.equal(listCalls, 2, 'core close does not add a second retention admission gate');

  db.close();
});
