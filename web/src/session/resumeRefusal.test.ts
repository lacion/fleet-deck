// DL-433: the refusal of a resume because a previous run is not proven ended.
import { describe, expect, test } from 'bun:test';
import { isUnprovenRun, MARK_UNCONFIRMED, markAndResume, withResumeCheck } from './resumeRefusal';
import { type Outcome, sessionActions } from './commands';

describe('isUnprovenRun', () => {
  test('true for the refusal of an unproven previous run', () => {
    expect(
      isUnprovenRun(
        'Refused: not_resumable: the previous execution may still run (shim x is exited (exit_unverified))',
      ),
    ).toBe(true);
  });

  test('false for another refusal', () => {
    expect(isUnprovenRun('Refused: only an ended session can be resumed')).toBe(false);
    expect(
      isUnprovenRun(
        'Refused: not_resumable: another execution of this conversation may still run (shim y)',
      ),
    ).toBe(false);
  });
});

describe('markAndResume', () => {
  const run = (marked: Outcome, resumed: Outcome) => {
    const calls: string[] = [];
    const actions = {
      reconcileExecution: async (id: string) => {
        calls.push(`reconcile ${id}`);
        return marked;
      },
      resume: async () => {
        calls.push('resume');
        return resumed;
      },
    };
    return { calls, result: markAndResume(actions, 'shim-1') };
  };

  test('it marks, then resumes', async () => {
    const t = run({ ok: true, text: 'Done.' }, { ok: true, text: 'Done.' });
    expect(await t.result).toEqual({ ok: true, text: 'Done.' });
    expect(t.calls).toEqual(['reconcile shim-1', 'resume']);
  });

  test('a refused mark does not resume and returns its text', async () => {
    const t = run({ ok: false, text: 'Refused: a process lives' }, { ok: true, text: 'Done.' });
    expect(await t.result).toEqual({ ok: false, text: 'Refused: a process lives' });
    expect(t.calls).toEqual(['reconcile shim-1']);
  });

  test('a refused resume returns its text', async () => {
    const t = run({ ok: true, text: 'Done.' }, { ok: false, text: 'Refused: busy' });
    expect(await t.result).toEqual({ ok: false, text: 'Refused: busy' });
  });

  test('a pending mark resumes only when the station confirms it (G4)', async () => {
    const pending = (final: Outcome | null): Outcome => ({
      ok: true,
      text: 'Sent.',
      later: async () => final,
    });
    const done = { ok: true, text: 'Done.' };
    const confirmed = run(pending(done), done);
    expect(await confirmed.result).toEqual(done);
    expect(confirmed.calls).toEqual(['reconcile shim-1', 'resume']);
    const refused = run(pending({ ok: false, text: 'Refused: a process lives' }), done);
    expect(await refused.result).toEqual({ ok: false, text: 'Refused: a process lives' });
    expect(refused.calls).toEqual(['reconcile shim-1']);
    const lost = run(pending(null), done);
    expect(await lost.result).toEqual({ ok: false, text: MARK_UNCONFIRMED });
    expect(lost.calls).toEqual(['reconcile shim-1']);
  });
});

describe('withResumeCheck', () => {
  const refusal = 'Refused: not_resumable: the previous execution may still run (shim x)';
  const wrap = (scopes: string[], outcome: Outcome) => {
    const base = sessionActions(async () => ({}), { station_id: 's', session_id: 'a' }, scopes);
    let opened = 0;
    const actions = withResumeCheck({ ...base, resume: async () => outcome }, () => {
      opened += 1;
    });
    return { actions, opened: () => opened };
  };

  test('the owner gets the dialog on the refusal and still the text', async () => {
    const t = wrap(['admin'], { ok: false, text: refusal });
    expect(await t.actions.resume()).toEqual({ ok: false, text: refusal });
    expect(t.opened()).toBe(1);
  });

  test('a member sees only the refusal', async () => {
    const t = wrap(['read', 'drive'], { ok: false, text: refusal });
    expect(await t.actions.resume()).toEqual({ ok: false, text: refusal });
    expect(t.opened()).toBe(0);
  });

  test('another refusal or an accepted resume opens nothing', async () => {
    const other = wrap(['admin'], {
      ok: false,
      text: 'Refused: only an ended session can be resumed',
    });
    await other.actions.resume();
    const done = wrap(['admin'], { ok: true, text: 'Done.' });
    await done.actions.resume();
    expect(other.opened() + done.opened()).toBe(0);
  });
});
