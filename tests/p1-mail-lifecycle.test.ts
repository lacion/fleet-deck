import assert from 'node:assert/strict';
import { openDb } from '../src/daemon/db.ts';
import { createMail } from '../src/daemon/mail.ts';
import { createStatements } from '../src/daemon/statements.ts';
import test from './helpers/harness-test.ts';

const SID = 'mail-lifecycle-session';
const WINDOW = {
  window: 'fd4711-mail-lifecycle',
  window_id: '@mail-lifecycle',
  pane_dead: false,
};

type MailContext = Parameters<typeof createMail>[0];

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

interface HarnessOptions {
  graceMs?: number;
  findScopedWindow?: MailContext['findScopedWindow'];
  paneCurrentCommand?: MailContext['tmuxAdapter']['paneCurrentCommand'];
  pasteText?: MailContext['tmuxAdapter']['pasteText'];
  sendEnter?: MailContext['tmuxAdapter']['sendEnter'];
}

function mailHarness(options: HarnessOptions = {}) {
  const db = openDb(':memory:');
  const { q } = createStatements(db);
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions
      (session_id, callsign, col, started_at, last_seen, source)
     VALUES (?, ?, 'idle', ?, ?, 'hooks')`,
  ).run(SID, 'heron-mail', now, now);
  db.prepare(
    `INSERT INTO spawns
      (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
     VALUES (?, ?, ?, ?, ?, ?, 'live')`,
  ).run('spawn-mail-lifecycle', SID, 'heron-mail', 'fleetdeck-4711', WINDOW.window, now);

  const calls = {
    find: 0,
    pane: 0,
    paste: 0,
    enter: 0,
    ticks: 0,
    logs: 0,
    mutations: 0,
  };
  const findScopedWindow: MailContext['findScopedWindow'] = async (name) => {
    calls.find++;
    if (options.findScopedWindow) return options.findScopedWindow(name);
    return WINDOW;
  };
  const tmuxAdapter: MailContext['tmuxAdapter'] = {
    paneCurrentCommand: async (target) => {
      calls.pane++;
      if (options.paneCurrentCommand) return options.paneCurrentCommand(target);
      return { dead: false, cmd: 'claude' };
    },
    pasteText: async (target, text) => {
      calls.paste++;
      if (options.pasteText) return options.pasteText(target, text);
      return true;
    },
    sendEnter: async (target) => {
      calls.enter++;
      if (options.sendEnter) return options.sendEnter(target);
      return true;
    },
  };
  const api = createMail({
    db,
    q,
    tick: () => {
      calls.ticks++;
    },
    logEvent: () => {
      calls.logs++;
    },
    onMutate: () => {
      calls.mutations++;
    },
    questions: { pendingOf: () => [] },
    tmuxAdapter,
    findScopedWindow,
    scopedPaneTarget: (win) => win.window_id,
    PANE_MAIL_GRACE_MS: options.graceMs ?? 60_000,
    MAIL_CLAIM_LEASE_MS: 60_000,
  });
  let dbClosed = false;
  return {
    api,
    calls,
    db,
    closeDb: () => {
      if (dbClosed) return;
      dbClosed = true;
      db.close();
    },
  };
}

test('P1 mail lifecycle quiesces synchronously, cancels grace timers, and closes idempotently', async (t) => {
  const harness = mailHarness({ graceMs: 20 });
  const { api, calls, db } = harness;
  t.after(async () => {
    await api.mailLifecycle.close();
    harness.closeDb();
  });

  assert.equal(api.mail(SID, 'ops', 'queued before quiesce').refused, undefined);
  assert.equal(api.mailLifecycle.quiesce(), true);
  assert.equal(
    api.mailLifecycle.quiesce(),
    false,
    'the admission latch is synchronous and one-way',
  );

  const before = db.prepare<{ n: number }>('SELECT COUNT(*) AS n FROM mail').get()?.n;
  const refused = api.mail(SID, 'ops', 'late direct insert');
  assert.equal(refused.refused, true);
  assert.match(refused.reason ?? '', /quiescing/);
  assert.equal(
    db.prepare<{ n: number }>('SELECT COUNT(*) AS n FROM mail').get()?.n,
    before,
    'post-quiesce direct mail never reaches SQLite',
  );

  const closeA = api.mailLifecycle.close();
  const closeB = api.mailLifecycle.close();
  assert.strictEqual(closeB, closeA, 'double close shares one settlement promise');
  await closeA;
  assert.strictEqual(
    api.mailLifecycle.close(),
    closeA,
    'closed lifecycle retains promise identity',
  );

  harness.closeDb();
  assert.equal(api.mail(SID, 'ops', 'after DB close').refused, true);
  assert.equal((await api.postMail({ to: SID, from: 'ops', text: 'late post' })).status, 503);
  assert.equal(await api.tryOwnedPaneDelivery(SID), false);

  await delay(70);
  assert.deepEqual(
    { find: calls.find, pane: calls.pane, paste: calls.paste, enter: calls.enter },
    { find: 0, pane: 0, paste: 0, enter: 0 },
    'the cancelled grace timer and refused late work run no tmux callback after DB close',
  );
});

test('P1 mail close joins an admitted postMail probe and suppresses its late insert', async (t) => {
  const windowGate = deferred<typeof WINDOW>();
  const probeStarted = deferred<void>();
  const harness = mailHarness({
    findScopedWindow: async () => {
      probeStarted.resolve();
      return windowGate.promise;
    },
  });
  const { api, calls, db } = harness;
  t.after(async () => {
    windowGate.resolve(WINDOW);
    await api.mailLifecycle.close();
    harness.closeDb();
  });

  const posting = api.postMail({ to: SID, from: 'ops', text: 'blocked route probe' });
  await probeStarted.promise;
  const closing = api.mailLifecycle.close();
  let closeSettled = false;
  void closing.then(() => {
    closeSettled = true;
  });
  await Promise.resolve();
  assert.equal(closeSettled, false, 'close waits for the already-admitted postMail operation');
  assert.equal(db.prepare<{ n: number }>('SELECT COUNT(*) AS n FROM mail').get()?.n, 0);

  windowGate.resolve(WINDOW);
  const result = await posting;
  assert.equal(result.status, 503, 'the resumed route probe is refused after quiesce');
  await closing;
  assert.equal(closeSettled, true);
  assert.equal(
    db.prepare<{ n: number }>('SELECT COUNT(*) AS n FROM mail').get()?.n,
    0,
    'the late continuation never inserted mail',
  );
  assert.equal(calls.ticks, 0);
  assert.equal(calls.mutations, 0);

  harness.closeDb();
  assert.equal((await api.postMail({ to: SID, from: 'ops', text: 'after close' })).status, 503);
});

test('P1 mail close joins an in-flight paste and leaves its lease as the restart recovery boundary', async (t) => {
  const pasteGate = deferred<boolean>();
  const pasteStarted = deferred<void>();
  const harness = mailHarness({
    graceMs: 30,
    pasteText: async () => {
      pasteStarted.resolve();
      return pasteGate.promise;
    },
  });
  const { api, calls, db } = harness;
  t.after(async () => {
    pasteGate.resolve(false);
    await api.mailLifecycle.close();
    harness.closeDb();
  });

  api.mail(SID, 'ops', 'lease survives shutdown');
  const delivery = api.tryOwnedPaneDelivery(SID);
  await pasteStarted.promise;
  const claimedBefore = db
    .prepare<{ claimed_at: number | null }>('SELECT claimed_at FROM mail LIMIT 1')
    .get()?.claimed_at;
  assert.ok(claimedBefore, 'delivery acquired its restart-recoverable lease before pasting');

  const closing = api.mailLifecycle.close();
  let closeSettled = false;
  void closing.then(() => {
    closeSettled = true;
  });
  await Promise.resolve();
  assert.equal(closeSettled, false, 'close joins the non-cancellable paste callback');

  pasteGate.resolve(false);
  assert.equal(await delivery, false);
  await closing;
  const row = db
    .prepare<{ claimed_at: number | null; delivered_at: number | null }>(
      'SELECT claimed_at, delivered_at FROM mail LIMIT 1',
    )
    .get();
  assert.equal(
    row?.claimed_at,
    claimedBefore,
    'the post-quiesce continuation did not release the lease through SQLite',
  );
  assert.equal(row?.delivered_at, null);
  assert.equal(calls.mutations, 0, 'no late mutation callback ran');

  const callsAtClose = { ...calls };
  harness.closeDb();
  await delay(70);
  assert.deepEqual(calls, callsAtClose, 'the cleared grace timer fires no callback after close');
});

test('P1 mail close suppresses acknowledgement and event callbacks after an in-flight Enter', async (t) => {
  const enterGate = deferred<boolean>();
  const enterStarted = deferred<void>();
  const harness = mailHarness({
    sendEnter: async () => {
      enterStarted.resolve();
      return enterGate.promise;
    },
  });
  const { api, calls, db } = harness;
  t.after(async () => {
    enterGate.resolve(true);
    await api.mailLifecycle.close();
    harness.closeDb();
  });

  api.mail(SID, 'ops', 'enter is in flight');
  const delivery = api.tryOwnedPaneDelivery(SID);
  await enterStarted.promise;
  const claimedBefore = db
    .prepare<{ claimed_at: number | null }>('SELECT claimed_at FROM mail LIMIT 1')
    .get()?.claimed_at;
  assert.ok(claimedBefore);

  const closing = api.mailLifecycle.close();
  enterGate.resolve(true);
  assert.equal(await delivery, false, 'shutdown suppresses the late delivery acknowledgement');
  await closing;

  const row = db
    .prepare<{ claimed_at: number | null; delivered_at: number | null }>(
      'SELECT claimed_at, delivered_at FROM mail LIMIT 1',
    )
    .get();
  assert.equal(row?.claimed_at, claimedBefore);
  assert.equal(
    row?.delivered_at,
    null,
    'the resumed Enter callback did not acknowledge via SQLite',
  );
  assert.equal(calls.ticks, 0);
  assert.equal(calls.logs, 0);
  assert.equal(calls.mutations, 0);
});
