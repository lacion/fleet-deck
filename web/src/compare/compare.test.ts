import { describe, expect, test } from 'bun:test';
import type { DeckSession, Event, SessionKey } from '../gen';
import { ApiError } from '../runtime/api';
import { card } from '../testing/fleet';
import { envelopes } from '../testing/timeline';
import {
  batchCountOf,
  batchCounts,
  batchRef,
  digestOf,
  isEnded,
  KEEP_WAIT_MS,
  type KeepDeps,
  type KeepProgress,
  keepConfirm,
  keepOne,
  lastAgentMessage,
  newBatchId,
  patchFiles,
  raceSummary,
  siblings,
} from './compare';

const member = (id: string, batch: string, index: number, extra = {}): DeckSession =>
  card(id, 'idle', 'station-a', { batch: batchRef(batch, index, 3), ...extra });

describe('batch ids', () => {
  test('a batch id is a ULID: 26 Crockford characters, the time first', () => {
    const id = newBatchId(1_760_000_000_000, () => 0);
    expect(id).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/);
    expect(id.endsWith('0'.repeat(16))).toBe(true);
    // A later time sorts after an earlier one.
    expect(newBatchId(1_760_000_000_001, () => 0) > id).toBe(true);
    // The station accepts it: 1 to 64 characters of [0-9A-Za-z_-].
    expect(id).toMatch(/^[0-9A-Za-z_-]{1,64}$/);
  });
  test('the randomness fills the last 16 characters', () => {
    const id = newBatchId(0, () => 0.999);
    expect(id.slice(10)).toBe('Z'.repeat(16));
    expect(id.slice(0, 10)).toBe('0'.repeat(10));
  });
});

describe('siblings', () => {
  const fleet = [
    member('c', 'b1', 3),
    member('a', 'b1', 1),
    member('x', 'b2', 1),
    member('b', 'b1', 2),
    card('plain'),
  ];
  test('the sessions of one batch come in the order of their index', () => {
    expect(siblings(fleet, 'b1').map((s) => s.summary.session_id)).toEqual(['a', 'b', 'c']);
    expect(siblings(fleet, 'none')).toEqual([]);
  });
  test('only a batch with two or more sessions has a count', () => {
    const counts = batchCounts(fleet);
    expect(counts.get('b1')).toBe(3);
    expect(counts.has('b2')).toBe(false);
    expect(batchCountOf(fleet[0] as DeckSession, counts)).toBe(3);
    expect(batchCountOf(fleet[2] as DeckSession, counts)).toBe(0);
    expect(batchCountOf(fleet[4] as DeckSession, counts)).toBe(0);
  });
});

describe('what a column shows', () => {
  const text = (value: string, turn = 'turn-1'): Event => ({
    kind: 'message.completed',
    turn_id: turn,
    parent_node_id: null,
    text: value,
  });
  test('the last agent message is the newest message of the top level', () => {
    expect(lastAgentMessage(envelopes('s', [text('first'), text('second')]))).toBe('second');
    expect(lastAgentMessage(envelopes('s', []))).toBeNull();
  });
  test('a digest is read as a string or as an object with text, and nothing else', () => {
    const base = card('d').summary;
    expect(digestOf(base)).toBeNull();
    expect(digestOf({ ...base, digest: 'Added tests.' } as never)).toBe('Added tests.');
    expect(digestOf({ ...base, digest: { text: 'Fixed it.' } } as never)).toBe('Fixed it.');
    expect(digestOf({ ...base, digest: '  ' } as never)).toBeNull();
    expect(digestOf({ ...base, digest: 7 } as never)).toBeNull();
  });
  test('the changed files come from the diff headers, each once', () => {
    const patch = [
      'diff --git a/src/a.ts b/src/a.ts',
      '+x',
      'diff --git a/README.md b/README.md',
      'diff --git a/src/a.ts b/src/a.ts',
    ].join('\n');
    expect(patchFiles(patch)).toEqual(['src/a.ts', 'README.md']);
    expect(patchFiles('')).toEqual([]);
  });
  test('the confirm text names the kept session and the count', () => {
    expect(keepConfirm('Fix login', 2)).toBe(
      'Keep Fix login? The other 2 sessions are stopped and their worktrees move to the trash; nothing is deleted.',
    );
    expect(keepConfirm('Fix login', 1)).toContain('The other 1 session is stopped');
  });
});

describe('keep this one', () => {
  const key = (id: string): SessionKey => ({ station_id: 'station-a', session_id: id });
  type State = { state: 'running' | 'stopping' | 'exited'; worktree: string | null };
  function world(states: Record<string, State>, over: Partial<KeepDeps> = {}) {
    const calls: string[] = [];
    const deps: KeepDeps = {
      summary: (k) => {
        const s = states[k.session_id];
        return s === undefined
          ? null
          : ({ session_id: k.session_id, state: s.state, worktree: s.worktree } as never);
      },
      stop: async (k) => {
        calls.push(`stop ${k.session_id}`);
        // The process ends one look later.
        const s = states[k.session_id];
        if (s !== undefined) s.state = 'exited';
        return { ok: true, text: 'Done.' };
      },
      trash: async (k) => {
        calls.push(`trash ${k.session_id}`);
        return { ok: true, text: 'Moving the worktree to the trash…' };
      },
      sleep: async () => {},
      ...over,
    };
    const last = new Map<string, KeepProgress>();
    const report = (p: KeepProgress) => last.set(p.key.session_id, p);
    return { calls, deps, last, report };
  }

  test('a live session is stopped, then its worktree is trashed', async () => {
    const w = world({ a: { state: 'running', worktree: '/w/a' } });
    await keepOne([key('a')], w.deps, w.report);
    expect(w.calls).toEqual(['stop a', 'trash a']);
    expect(w.last.get('a')?.step).toBe('done');
  });
  test('an ended session is only trashed; a session without a worktree is only stopped', async () => {
    const w = world({
      a: { state: 'exited', worktree: '/w/a' },
      b: { state: 'running', worktree: null },
    });
    await keepOne([key('a'), key('b')], w.deps, w.report);
    expect(w.calls.sort()).toEqual(['stop b', 'trash a']);
    expect(w.last.get('b')?.text).toContain('no worktree');
  });
  test('a refused stop is shown and the worktree stays', async () => {
    const w = world(
      { a: { state: 'running', worktree: '/w/a' }, b: { state: 'exited', worktree: '/w/b' } },
      { stop: async () => ({ ok: false, text: 'Refused: not allowed' }) },
    );
    await keepOne([key('a'), key('b')], w.deps, w.report);
    expect(w.calls).toEqual(['trash b']);
    expect(w.last.get('a')).toMatchObject({ step: 'failed', text: 'Refused: not allowed' });
    expect(w.last.get('b')?.step).toBe('done');
  });
  test('a refused trash is shown per session', async () => {
    const w = world(
      { a: { state: 'exited', worktree: '/w/a' } },
      { trash: async () => ({ ok: false, text: 'a terminal holds the session' }) },
    );
    await keepOne([key('a')], w.deps, w.report);
    expect(w.last.get('a')?.step).toBe('failed');
    expect(w.last.get('a')?.text).toContain('a terminal holds the session');
  });
  test('a session that does not end in time fails and is never trashed', async () => {
    let slept = 0;
    const w = world(
      { a: { state: 'running', worktree: '/w/a' } },
      {
        stop: async () => ({ ok: true, text: 'Done.' }),
        sleep: async (ms) => {
          slept += ms;
        },
      },
    );
    await keepOne([key('a')], w.deps, w.report);
    expect(slept).toBe(KEEP_WAIT_MS);
    expect(w.calls).toEqual([]);
    expect(w.last.get('a')?.step).toBe('failed');
  });
  test('a session that is stopping is not stopped again', async () => {
    const states: Record<string, State> = { a: { state: 'stopping', worktree: '/w/a' } };
    const w = world(states, {
      sleep: async () => {
        const s = states['a'];
        if (s !== undefined) s.state = 'exited';
      },
    });
    await keepOne([key('a')], w.deps, w.report);
    expect(w.calls).toEqual(['trash a']);
  });
  test('a throw is a failure of that session only', async () => {
    const w = world(
      { a: { state: 'exited', worktree: '/w/a' }, b: { state: 'exited', worktree: '/w/b' } },
      {
        trash: async (k) => {
          if (k.session_id === 'a') throw new Error('network down');
          return { ok: true, text: 'ok' };
        },
      },
    );
    await keepOne([key('a'), key('b')], w.deps, w.report);
    expect(w.last.get('a')).toMatchObject({ step: 'failed', text: 'network down' });
    expect(w.last.get('b')?.step).toBe('done');
  });
  test('a session that left the board fails', async () => {
    const w = world({});
    await keepOne([key('gone')], w.deps, w.report);
    expect(w.last.get('gone')?.step).toBe('failed');
  });
  test('ended means exited or lost', () => {
    const s = card('e').summary;
    expect(isEnded({ ...s, state: 'exited' })).toBe(true);
    expect(isEnded({ ...s, state: 'lost' })).toBe(true);
    expect(isEnded({ ...s, state: 'running' })).toBe(false);
    expect(isEnded({ ...s, state: 'stopping' })).toBe(false);
  });
});

describe('race summary (R10.5)', () => {
  const keys = [
    { station_id: 'station-a', session_id: 'a' },
    { station_id: 'station-a', session_id: 'b' },
  ];
  test('the answer text is shown', async () => {
    const seen: { path: string; body: unknown }[] = [];
    const out = await raceSummary(async (path, body) => {
      seen.push({ path, body });
      return { text: 'A edited two files.' };
    }, keys);
    expect(out).toEqual({ kind: 'text', text: 'A edited two files.' });
    expect(seen).toEqual([
      {
        path: '/api/assist/race',
        body: {
          sessions: [
            { station_id: 'station-a', session_id: 'a' },
            { station_id: 'station-a', session_id: 'b' },
          ],
        },
      },
    ]);
  });
  test('a 404 hides the slot; another error is shown; an unreadable answer is an error', async () => {
    expect(
      await raceSummary(async () => {
        throw new ApiError(404, 'Request failed (404)');
      }, keys),
    ).toEqual({ kind: 'absent' });
    expect(
      await raceSummary(async () => {
        throw new SyntaxError('Unexpected token');
      }, keys),
    ).toEqual({ kind: 'absent' });
    expect(
      await raceSummary(async () => {
        throw new ApiError(502, 'the station refused');
      }, keys),
    ).toEqual({ kind: 'error', text: 'the station refused' });
    expect((await raceSummary(async () => ({ nope: 1 }), keys)).kind).toBe('error');
  });
});
