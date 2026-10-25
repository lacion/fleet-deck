// B20 (design `usage-cost.md` §4.4): the recount row of the Usage tab, its flow and its result line.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, SessionKey, SessionSummary } from '../gen';
import { card } from '../testing/fleet';
import { newUsage } from '../testing/summary';
import {
  CONFIRM_TEXT,
  CONFIRM_TITLE,
  eligibleSessions,
  type RecountEntry,
  type RecountRun,
  recountFlow,
  resultLine,
  runRecounts,
  UsageRecount,
} from './UsageRecount';
import { UsageMeters } from './UsageMeters';
import type { RecountCall, RecountResult } from './usageRecountCall';

const OLD = { input_tokens: 4000, output_tokens: 90, cost_usd: 12, cost_inferred: false };

/** A session whose whole total is an old count. */
const oldCount = (
  id: string,
  state: SessionSummary['state'] = 'exited',
  extra: Partial<SessionSummary> = {},
  station = 'station-a',
): DeckSession => card(id, 'ended', station, { usage: OLD, state, ...extra });

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const keyOf = (s: DeckSession): SessionKey => ({
  station_id: s.station_id,
  session_id: s.summary.session_id,
});
const html = (sessions: DeckSession[], initial?: RecountRun) =>
  renderToStaticMarkup(
    <UsageRecount
      sessions={sessions}
      recount={async (): Promise<RecountResult> => ({ kind: 'failed', text: 'x' })}
      initial={initial}
    />,
  );

/** A recount call that records its keys and answers `accepted`. */
function recorder() {
  const calls: string[] = [];
  const call: RecountCall = async (key) => {
    calls.push(key.session_id);
    return { kind: 'accepted', commandId: `c-${key.session_id}` };
  };
  return { calls, call };
}

describe('the recount row (usage-cost §4.4)', () => {
  test('the_row_is_absent_without_old_counts', () => {
    expect(html([])).toBe('');
    expect(
      html([card('n', 'working', 'station-a', { usage: newUsage({ input_tokens: 5 }) })]),
    ).toBe('');
    // A Codex or Claude session that has no usage yet is a new session: no old count.
    expect(html([card('e')])).toBe('');
    // A provider that is not Claude or Codex has no record to recount.
    expect(html([oldCount('x', 'exited', { provider: null })])).toBe('');
  });

  test('the_row_is_absent_without_the_call', () => {
    const sessions = [oldCount('a')];
    const without = renderToStaticMarkup(<UsageMeters sessions={sessions} stations={[]} />);
    expect(without).not.toContain('Recount from provider records');
    const call = async (): Promise<RecountResult> => ({ kind: 'not_supported' });
    const withCall = renderToStaticMarkup(
      <UsageMeters sessions={sessions} stations={[]} recount={call} />,
    );
    expect(withCall).toContain('<dt class="fd-label">Old counts</dt>');
    expect(withCall).toContain('>Recount from provider records</button>');
  });

  test('cancel_sends_nothing', async () => {
    const { calls, call } = recorder();
    const runs: RecountRun[] = [];
    const flow = recountFlow(call, (run) => runs.push(run));
    // Confirm without a question sends nothing.
    await flow.confirm([oldCount('a')]);
    expect(calls).toEqual([]);
    flow.ask();
    expect(flow.get().phase).toBe('confirm');
    flow.cancel();
    expect(flow.get().phase).toBe('idle');
    await flow.confirm([oldCount('a')]);
    expect(calls).toEqual([]);
    // The dialog: its title and description, the Recount and Cancel buttons.
    const dialog = html([oldCount('a')], { phase: 'confirm', finished: 0, total: 0, entries: [] });
    expect(dialog).toContain(`<h2>${CONFIRM_TITLE}</h2>`);
    expect(CONFIRM_TITLE).toBe('Recount the old counts?');
    expect(dialog).toContain(CONFIRM_TEXT.replace(/"/g, '&quot;'));
    expect(dialog).toContain('class="fd-primary"');
    expect(dialog).toContain('>Recount</button>');
    expect(dialog).toContain('>Cancel</button>');
    expect(html([oldCount('a')])).not.toContain('role="dialog"');
  });

  test('recount_sends_one_command_per_ended_old_count_session_and_skips_running_ones', async () => {
    const { calls, call } = recorder();
    const sessions = [
      oldCount('exited'),
      oldCount('lost', 'lost', { provider: 'codex' }),
      oldCount('running', 'running'),
      oldCount('stopping', 'stopping'),
      card('new', 'ended', 'station-a', { state: 'exited', usage: newUsage({ input_tokens: 1 }) }),
    ];
    expect(eligibleSessions(sessions).ended.map((s) => s.summary.session_id)).toEqual([
      'exited',
      'lost',
    ]);
    expect(eligibleSessions(sessions).running.map((s) => s.summary.session_id)).toEqual([
      'running',
      'stopping',
    ]);
    const flow = recountFlow(call, () => {});
    flow.ask();
    await flow.confirm(sessions);
    expect(calls.sort()).toEqual(['exited', 'lost']);
    const run = flow.get();
    expect(run.phase).toBe('done');
    expect(run.entries.filter((e) => !e.sent)).toHaveLength(2);
    expect(run.entries.filter((e) => e.sent)).toHaveLength(2);
    // The two that may still run are in the result line; the sent ones wait for their totals.
    expect(resultLine(run.entries, sessions)).toBe(
      '2 wait for their new totals. 2 may still run and keep their old counts.',
    );
  });

  test('a_result_waits_for_the_matching_summary', () => {
    const entries: RecountEntry[] = [
      {
        key: { station_id: 'station-a', session_id: 'a' },
        sent: true,
        result: { kind: 'accepted', commandId: 'c1' },
      },
    ];
    const recount = (
      command_id: string,
      source: 'claude_transcript' | 'codex_rollout' | 'none',
    ) => ({
      at: 1,
      source,
      command_id,
      previous: OLD,
    });
    const line = (extra: Partial<SessionSummary>) =>
      resultLine(entries, [oldCount('a', 'exited', extra)]);
    expect(line({})).toBe('1 wait for their new totals.');
    // Another command id is not this recount.
    expect(line({ usage_recount: recount('c0', 'claude_transcript') })).toBe(
      '1 wait for their new totals.',
    );
    expect(line({ usage_recount: recount('c1', 'claude_transcript') })).toBe(
      'Recounted 1 session.',
    );
    expect(line({ usage_recount: recount('c1', 'codex_rollout') })).toBe('Recounted 1 session.');
    expect(line({ usage_recount: recount('c1', 'none') })).toBe(
      '1 have no provider record: their totals are now unknown.',
    );
    // A pending command that later appears counts the same way.
    const pending: RecountEntry[] = [
      {
        key: { station_id: 'station-a', session_id: 'a' },
        sent: true,
        result: { kind: 'pending', commandId: 'c1' },
      },
    ];
    const done = oldCount('a', 'exited', { usage_recount: recount('c1', 'claude_transcript') });
    expect(resultLine(pending, [done])).toBe('Recounted 1 session.');
    expect(resultLine(pending, [oldCount('a')])).toBe('1 wait for their new totals.');
    // The result line is in the render, and the row stays while the line shows.
    const shown = html([done], { phase: 'done', finished: 1, total: 1, entries: pending });
    expect(shown).toContain('<p class="fd-note" role="status">Recounted 1 session.</p>');
  });

  test('the_result_line_counts_each_outcome', () => {
    const at = (station: string, id: string) => ({ station_id: station, session_id: id });
    const sent = (station: string, id: string, result: RecountResult): RecountEntry => ({
      key: at(station, id),
      sent: true,
      result,
    });
    const recount = (id: string, source: 'claude_transcript' | 'none') => ({
      usage_recount: { at: 1, source, command_id: `c-${id}`, previous: OLD },
    });
    const sessions = [
      oldCount('s1', 'exited', recount('s1', 'claude_transcript')),
      oldCount('s2', 'exited', recount('s2', 'none')),
      oldCount('s3'),
    ];
    const entries: RecountEntry[] = [
      sent('station-a', 's1', { kind: 'accepted', commandId: 'c-s1' }),
      sent('station-a', 's2', { kind: 'accepted', commandId: 'c-s2' }),
      sent('station-a', 's3', { kind: 'accepted', commandId: 'c-s3' }),
      { key: at('station-a', 's4'), sent: false },
      sent('station-a', 's5', { kind: 'refused', code: 'session_running', message: 'running' }),
      sent('station-a', 's6', { kind: 'refused', code: 'other_history', message: 'history' }),
      sent('station-a', 's7', { kind: 'refused', code: 'origin_unproven', message: 'origin' }),
      sent('station-b', 's8', { kind: 'not_supported' }),
      sent('station-b', 's9', { kind: 'not_supported' }),
      sent('station-c', 's10', { kind: 'not_supported' }),
      sent('station-a', 's11', { kind: 'refused', code: 'refused', message: 'boom' }),
      sent('station-a', 's12', { kind: 'failed', text: 'Not delivered: link down' }),
    ];
    expect(resultLine(entries, sessions)).toBe(
      'Recounted 1 session. ' +
        '1 have no provider record: their totals are now unknown. ' +
        '1 wait for their new totals. ' +
        '2 may still run and keep their old counts. ' +
        '1 hold usage of another session in their record and keep their old counts. ' +
        '1 have no proof that their conversation started in Fleet Deck and keep their old counts. ' +
        // The 409 is counted per station: two stations, three sessions.
        '2 stations need a newer Fleet Deck. ' +
        '2 could not be recounted (boom).',
    );
    expect(resultLine([], sessions)).toBe('');
    const many = [1, 2].map((n) =>
      sent('station-a', `m${n}`, { kind: 'accepted', commandId: `c-m${n}` }),
    );
    const done = [1, 2].map((n) =>
      oldCount(`m${n}`, 'exited', recount(`m${n}`, 'claude_transcript')),
    );
    expect(resultLine(many, done)).toBe('Recounted 2 sessions.');
  });

  test('at_most_four_commands_run_at_once', async () => {
    let live = 0;
    let peak = 0;
    let calls = 0;
    const gates: (() => void)[] = [];
    const call: RecountCall = (key) => {
      live += 1;
      calls += 1;
      peak = Math.max(peak, live);
      return new Promise((resolve) => {
        gates.push(() => {
          live -= 1;
          resolve({ kind: 'accepted', commandId: key.session_id });
        });
      });
    };
    const sessions = Array.from({ length: 10 }, (_, i) => oldCount(`s${i}`));
    const progress: number[] = [];
    const run = runRecounts(sessions.map(keyOf), call, (n) => progress.push(n));
    await flush();
    expect(gates).toHaveLength(4);
    expect(peak).toBe(4);
    while (gates.length > 0) {
      gates.shift()?.();
      await flush();
    }
    const results = await run;
    expect(calls).toBe(10);
    expect(peak).toBe(4);
    expect(results).toHaveLength(10);
    expect(progress).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    // The button says how far the run is.
    const running = html(sessions, { phase: 'running', finished: 2, total: 5, entries: [] });
    expect(running).toContain('Recounting 2 of 5');
    expect(running).not.toContain('>Recount from provider records</button>');
  });
});
