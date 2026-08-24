import assert from 'node:assert/strict';
import { openDb } from '../src/daemon/db.ts';
import { createMail } from '../src/daemon/mail.ts';
import { runControlDetached } from '../src/daemon/platform/bun/ingress-supervisor-live.ts';
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
  // P9.4 Slice 1: inject the ingress runner so tryOwnedPaneDelivery discharges
  // through the Effect core; omit → the legacy tryOwnedPaneDeliveryImpl body.
  // `| undefined` so the CORE_VARIANTS legacy row (runner: undefined) can be
  // passed explicitly under exactOptionalPropertyTypes; mailHarness re-narrows
  // it back to a conditional spread before reaching createMail's stricter field.
  runControlDetached?: typeof runControlDetached | undefined;
}

// P9.4 Slice 1: the two composers the delivery pins run against. `effect`
// injects the ingress runner (Effect core); `legacy` omits it (legacy body).
// Both must produce identical delivery bytes — that is the whole point of the
// parity harness (draft §3 Slice 1, precondition of Slice 2 per §6 Q5).
const CORE_VARIANTS = [
  { label: 'effect', runner: runControlDetached },
  { label: 'legacy', runner: undefined },
] as const;

// Finding 1: byte-identical outcomes cannot catch a dead dispatcher. Pin the
// spy: Effect core must invoke the injected runner; the legacy row must not.
function assertDispatcherLiveness(
  label: (typeof CORE_VARIANTS)[number]['label'],
  runnerCalls: number,
  expectedEffectCalls: number,
): void {
  if (label === 'effect') {
    assert.equal(
      runnerCalls,
      expectedEffectCalls,
      `${label}: runControlDetached invoked ${String(expectedEffectCalls)} time(s) — a dead dispatcher would be 0`,
    );
  } else {
    assert.equal(runnerCalls, 0, `${label}: legacy omits the runner`);
  }
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
    // Finding 1: dispatcher-liveness. Incremented only when the injected
    // runner is actually invoked; stays 0 on the legacy row (no runner).
    runner: 0,
  };
  // Byte-parity call log: the ordered tmux side effects with their args, the
  // p1 analog of daemon-maintenance's `state.calls` (:679,:708,:733). Both
  // composers must produce identical entries.
  const tmuxLog: Array<[string, ...unknown[]]> = [];
  // Wrap the injected runner so a dead dispatcher (Effect core ignored, always
  // the legacy twin) fails the suite. Outcomes are designed to be byte-identical,
  // so they cannot discriminate dispatcher selection on their own.
  const injectedRunner = options.runControlDetached;
  const runControlDetachedSpy: typeof runControlDetached | undefined = injectedRunner
    ? (effect) => {
        calls.runner++;
        return injectedRunner(effect);
      }
    : undefined;
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
      tmuxLog.push(['pasteText', target, text]);
      if (options.pasteText) return options.pasteText(target, text);
      return true;
    },
    sendEnter: async (target) => {
      calls.enter++;
      tmuxLog.push(['sendEnter', target]);
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
    // P9.4 Slice 1: omitted → legacy body; injected (via the counting spy) →
    // Effect core. Threaded the same way spawn-quiesce-cancel.test.ts:186
    // threads it into createCore.
    ...(runControlDetachedSpy ? { runControlDetached: runControlDetachedSpy } : {}),
    PANE_MAIL_GRACE_MS: options.graceMs ?? 60_000,
    MAIL_CLAIM_LEASE_MS: 60_000,
  });
  let dbClosed = false;
  return {
    api,
    calls,
    tmuxLog,
    q,
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

// P9.4 Slice 1 gap 3: the isOpen() mid-run gate — a quiesce landing while the
// non-cancellable paste is in flight leaves the lease intact and fires no
// mutation. Parameterized so BOTH composers walk the same gate (draft §3
// Slice 1 gap 3 / D2). Under `effect` the gate at mail.ts:576 lives inside the
// coarse run thunk of the discharged Effect; under `legacy` it is the same
// native `if` — identical observable.
for (const variant of CORE_VARIANTS) {
  test(`P1 mail close joins an in-flight paste and leaves its lease as the restart recovery boundary (${variant.label})`, async (t) => {
    const pasteGate = deferred<boolean>();
    const pasteStarted = deferred<void>();
    const harness = mailHarness({
      graceMs: 30,
      runControlDetached: variant.runner,
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
    assertDispatcherLiveness(variant.label, calls.runner, 1);
  });

  test(`P1 mail close suppresses acknowledgement and event callbacks after an in-flight Enter (${variant.label})`, async (t) => {
    const enterGate = deferred<boolean>();
    const enterStarted = deferred<void>();
    const harness = mailHarness({
      runControlDetached: variant.runner,
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
    assertDispatcherLiveness(variant.label, calls.runner, 1);
  });
}

// ---------------------------------------------------------------------------
// P9.4 Slice 1 gap 1: the runner-injected byte-parity delivery matrix. Today's
// delivery suites all call tryOwnedPaneDelivery with NO injected runner, so the
// Effect core would ship CI-unexercised (draft §6 Q5 — the precondition of
// Slice 2). These re-run every delivery outcome through BOTH composers and pin
// the exact boolean, the mail-row lease/finalize state, and the ordered tmux
// side effects. They mirror daemon-maintenance.test.ts:662,711 (which run only
// the legacy body through createCore) at the createMail seam.
//
// Frame: `[FLEETDECK MAIL from <from_id>] <text>`; target: WINDOW.window_id via
// scopedPaneTarget.
const FRAME = (text: string): string => `[FLEETDECK MAIL from ops] ${text}`;
const TARGET = WINDOW.window_id;

for (const variant of CORE_VARIANTS) {
  test(`P1 owned-pane delivery (${variant.label}): success pastes, enters, and finalizes the lease`, async (t) => {
    const harness = mailHarness({ runControlDetached: variant.runner });
    const { api, calls, tmuxLog, db } = harness;
    t.after(async () => {
      await api.mailLifecycle.close();
      harness.closeDb();
    });

    api.mail(SID, 'ops', 'hello pane');
    assert.equal(await api.tryOwnedPaneDelivery(SID), true, `${variant.label}: delivered`);
    assert.deepEqual(
      tmuxLog,
      [
        ['pasteText', TARGET, FRAME('hello pane')],
        ['sendEnter', TARGET],
      ],
      `${variant.label}: paste then enter, same target and bytes`,
    );
    const row = db
      .prepare<{ claimed_at: number | null; delivered_at: number | null }>(
        'SELECT claimed_at, delivered_at FROM mail LIMIT 1',
      )
      .get();
    assert.ok(row?.delivered_at, `${variant.label}: confirmed Enter finalizes delivered_at`);
    assert.equal(row?.claimed_at, null, `${variant.label}: ackMail clears the lease`);
    assert.equal(calls.paste, 1);
    assert.equal(calls.enter, 1);
    assertDispatcherLiveness(variant.label, calls.runner, 1);
  });

  test(`P1 owned-pane delivery (${variant.label}): a registered watch waiter suppresses the paste`, async (t) => {
    const harness = mailHarness({ runControlDetached: variant.runner });
    const { api, calls, tmuxLog, db } = harness;
    t.after(async () => {
      await api.mailLifecycle.close();
      harness.closeDb();
    });

    api.addWatchWaiter(SID, () => {});
    api.mail(SID, 'ops', 'watcher first');
    assert.equal(await api.tryOwnedPaneDelivery(SID), false, `${variant.label}: watcher priority`);
    assert.deepEqual(tmuxLog, [], `${variant.label}: no tmux side effect`);
    assert.equal(calls.find, 0, `${variant.label}: the sync waiter gate short-circuits the probes`);
    const row = db
      .prepare<{ claimed_at: number | null; delivered_at: number | null }>(
        'SELECT claimed_at, delivered_at FROM mail LIMIT 1',
      )
      .get();
    assert.equal(row?.claimed_at, null, `${variant.label}: nothing was claimed`);
    assert.equal(row?.delivered_at, null);
    assertDispatcherLiveness(variant.label, calls.runner, 1);
  });

  test(`P1 owned-pane delivery (${variant.label}): a post-probe TOCTOU flip claims nothing (BUG-8)`, async (t) => {
    // The eligibility gate reads idle/queued BEFORE the awaited probes; a hook
    // flipping the card to 'working' during them must be caught by the fresh
    // ownedPaneRow re-read (mail.ts:563) — claim nothing, paste nothing.
    let dbRef: ReturnType<typeof mailHarness>['db'] | null = null;
    const harness = mailHarness({
      runControlDetached: variant.runner,
      paneCurrentCommand: async () => {
        dbRef?.prepare("UPDATE sessions SET col = 'working' WHERE session_id = ?").run(SID);
        return { dead: false, cmd: 'claude' };
      },
    });
    const { api, calls, tmuxLog, db } = harness;
    dbRef = db;
    t.after(async () => {
      await api.mailLifecycle.close();
      harness.closeDb();
    });

    api.mail(SID, 'ops', 'toctou');
    assert.equal(await api.tryOwnedPaneDelivery(SID), false, `${variant.label}: TOCTOU bail`);
    assert.deepEqual(tmuxLog, [], `${variant.label}: no paste after the re-read fails`);
    assert.equal(calls.pane, 1, `${variant.label}: the probe ran before the flip was observed`);
    const row = db
      .prepare<{ claimed_at: number | null; delivered_at: number | null }>(
        'SELECT claimed_at, delivered_at FROM mail LIMIT 1',
      )
      .get();
    assert.equal(row?.claimed_at, null, `${variant.label}: the lease txn never ran`);
    assert.equal(row?.delivered_at, null);
    assertDispatcherLiveness(variant.label, calls.runner, 1);
  });

  test(`P1 owned-pane delivery (${variant.label}): paste failure releases the lease and requeues`, async (t) => {
    const harness = mailHarness({
      runControlDetached: variant.runner,
      pasteText: async () => false,
    });
    const { api, calls, tmuxLog, db } = harness;
    t.after(async () => {
      await api.mailLifecycle.close();
      harness.closeDb();
    });

    api.mail(SID, 'ops', 'retry me');
    assert.equal(await api.tryOwnedPaneDelivery(SID), false, `${variant.label}: paste fail`);
    assert.deepEqual(
      tmuxLog,
      [['pasteText', TARGET, FRAME('retry me')]],
      `${variant.label}: pasted once, no Enter`,
    );
    const row = db
      .prepare<{ claimed_at: number | null; delivered_at: number | null }>(
        'SELECT claimed_at, delivered_at FROM mail LIMIT 1',
      )
      .get();
    assert.equal(row?.claimed_at, null, `${variant.label}: releaseClaim handed the lease back`);
    assert.equal(row?.delivered_at, null, `${variant.label}: never delivered — stays requeueable`);
    assert.equal(calls.enter, 0);
    assert.equal(calls.mutations, 1, `${variant.label}: exactly one requeue mutation`);
    assertDispatcherLiveness(variant.label, calls.runner, 1);
  });

  test(`P1 owned-pane delivery (${variant.label}): failed Enter finalizes without requeue (BUG-033)`, async (t) => {
    const harness = mailHarness({
      runControlDetached: variant.runner,
      sendEnter: async () => false,
    });
    const { api, calls, tmuxLog, db } = harness;
    t.after(async () => {
      await api.mailLifecycle.close();
      harness.closeDb();
    });

    api.mail(SID, 'ops', 'paste ok, enter fails');
    assert.equal(
      await api.tryOwnedPaneDelivery(SID),
      false,
      `${variant.label}: enter fail → false`,
    );
    assert.deepEqual(
      tmuxLog,
      [
        ['pasteText', TARGET, FRAME('paste ok, enter fails')],
        ['sendEnter', TARGET],
      ],
      `${variant.label}: pasted then attempted Enter`,
    );
    const row = db
      .prepare<{ claimed_at: number | null; delivered_at: number | null }>(
        'SELECT claimed_at, delivered_at FROM mail LIMIT 1',
      )
      .get();
    assert.ok(
      row?.delivered_at,
      `${variant.label}: the pasted text is the side effect — finalized, never re-pasted`,
    );
    assert.equal(row?.claimed_at, null);
    assert.equal(calls.logs, 1, `${variant.label}: one MailPaneEnterFailed event`);
    assert.equal(calls.mutations, 1);
    assertDispatcherLiveness(variant.label, calls.runner, 1);
  });

  // P9.4 Slice 1 gap 2: the claimAllMail ROLLBACK path (mail.ts:531-538). No
  // suite reaches it today. Force q.claimMail.run to throw mid-batch; assert the
  // partial claim is rolled back (both rows handed back, claimed_at NULL) and the
  // rejection propagates — legacy throws; the Effect discharge dies → the runner
  // rejects the Promise → the arming timer's .catch swallows it (fail-open, D6).
  test(`P1 owned-pane delivery (${variant.label}): a mid-batch claim throw ROLLBACKs and rejects`, async (t) => {
    const harness = mailHarness({ runControlDetached: variant.runner });
    const { api, calls, tmuxLog, db, q } = harness;
    t.after(async () => {
      await api.mailLifecycle.close();
      harness.closeDb();
    });

    api.mail(SID, 'ops', 'row one');
    api.mail(SID, 'ops', 'row two');

    // Replace q.claimMail with a stand-in that delegates the first lease to the
    // real statement (a durable partial write) and throws on the second, so the
    // ROLLBACK has something to revert.
    const qMut = q as unknown as { claimMail: { run: (...args: unknown[]) => unknown } };
    const realClaim = qMut.claimMail;
    let claimCalls = 0;
    qMut.claimMail = {
      run: (...args: unknown[]) => {
        claimCalls += 1;
        if (claimCalls >= 2) throw new Error('claimMail boom');
        return realClaim.run(...args);
      },
    };

    await assert.rejects(
      api.tryOwnedPaneDelivery(SID),
      `${variant.label}: the claim throw propagates as a rejection`,
    );
    assert.equal(
      claimCalls,
      2,
      `${variant.label}: the batch reached the second row before failing`,
    );
    assert.deepEqual(tmuxLog, [], `${variant.label}: nothing was pasted`);
    const rows = db
      .prepare<{ claimed_at: number | null; delivered_at: number | null }>(
        'SELECT claimed_at, delivered_at FROM mail ORDER BY id',
      )
      .all();
    assert.equal(rows.length, 2);
    for (const row of rows) {
      assert.equal(row.claimed_at, null, `${variant.label}: ROLLBACK reverted the partial lease`);
      assert.equal(row.delivered_at, null);
    }
    assert.equal(calls.paste, 0);
    assertDispatcherLiveness(variant.label, calls.runner, 1);
  });
}
