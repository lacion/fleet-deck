import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { CheckpointSide, DeckSession, DiffSummary, Event } from '../gen';
import type { Timeline } from '../runtime/state';
import { claudeCaps, codexCaps } from '../testing/capabilities';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import { envelopes } from '../testing/timeline';
import { checkpointText } from './checkpoint';
import { sessionActions } from './commands';
import { type PaneTab, SessionView } from './SessionView';
import { foldText, foldTurns, quietTurn, turnFiles } from './turnFolds';
import type { FinalizedTurn } from './workspace';

const key = { station_id: 'station-a', session_id: 's1' };
const ok = (commit: string): CheckpointSide => ({ state: 'available', commit });
const gone = (reason: string): CheckpointSide => ({ state: 'unavailable', reason });
const CHANGED: DiffSummary = { files_changed: 1, insertions: 2, deletions: 1 };

function finalized(
  turn: string,
  start = ok(`${turn}-start`),
  end = ok(`${turn}-end`),
  diff: DiffSummary | null = CHANGED,
): Event {
  return {
    kind: 'turn.finalized',
    turn_id: turn,
    checkpoint: { worktree_id: 'wt', scope: 'tracked', start, end },
    diff_summary: end.state === 'available' ? diff : null,
  };
}

const codex = (extra: Partial<DeckSession['summary']> = {}): DeckSession => ({
  station_id: 'station-a',
  status: 'idle',
  summary: summary('s1', 'idle', 1, { provider: 'codex', capabilities: codexCaps(), ...extra }),
});

const timeline = (events: Event[]): Timeline => ({
  incarnation: 'inc',
  cursor: 0,
  events: envelopes('s1', events),
  freshness: 'live',
  error: null,
  ready: true,
});

const view = (s: DeckSession, events: Event[], tab: PaneTab = 'changes') =>
  renderToStaticMarkup(
    <SessionView
      layout="tablet"
      session={s}
      stations={[station()]}
      freshness="live"
      timeline={timeline(events)}
      actions={sessionActions(
        async () => ({}),
        key,
        ['read', 'drive', 'approve'],
        undefined,
        async () => ({}),
      )}
      tab={tab}
    />,
  );

/** A finalized turn of the list, as `finalizedTurns` builds it. */
const turn = (
  id: string,
  start: CheckpointSide | null,
  end: CheckpointSide | null,
  diff: DiffSummary | null = null,
  reverted = false,
): FinalizedTurn => ({ turnId: id, start, end, diff, reverted });

const rollbackCaps = codexCaps();
const noRollbackCaps = claudeCaps();
/** A quiet turn for a provider with rollback: no start snapshot, so no enabled revert. */
const quiet = (id: string) => turn(id, gone('background_writers'), gone('background_writers'));

describe('turns without a snapshot (B23, DL-502, DL-503)', () => {
  test('turnFiles: a missing start snapshot is "no snapshot", not "no changes"', () => {
    expect(turnFiles(turn('t1', gone('provider_initiated'), ok('e')))).toEqual({
      kind: 'no_snapshot',
      reason: 'provider_initiated',
    });
    expect(turnFiles(turn('t1', ok('s'), gone('queued_input')))).toEqual({
      kind: 'no_snapshot',
      reason: 'queued_input',
    });
    expect(turnFiles(turn('t1', ok('s'), ok('e')))).toEqual({ kind: 'no_changes' });
    expect(turnFiles(turn('t1', ok('s'), ok('e'), CHANGED))).toEqual({ kind: 'changes' });
  });

  test('turnFiles: a turn without a checkpoint is "no snapshot" without a reason', () => {
    expect(turnFiles(turn('t1', null, null))).toEqual({ kind: 'no_snapshot', reason: null });
  });

  test('three quiet turns in a row fold; two do not', () => {
    const three = [quiet('t1'), quiet('t2'), quiet('t3'), quiet('t4')];
    const folds = foldTurns(three, rollbackCaps);
    expect(folds.map((e) => e.kind)).toEqual(['fold', 'turn']);
    const two = [quiet('t1'), quiet('t2'), quiet('t3')];
    expect(foldTurns(two, rollbackCaps).map((e) => e.kind)).toEqual(['turn', 'turn', 'turn']);
  });

  test('the last turn never folds', () => {
    const entries = foldTurns([quiet('t1'), quiet('t2'), quiet('t3'), quiet('t4')], rollbackCaps);
    expect(entries).toHaveLength(2);
    const [fold, last] = entries;
    expect(fold?.kind).toBe('fold');
    if (fold?.kind === 'fold') {
      expect([fold.first, fold.last]).toEqual([0, 2]);
      expect(fold.turns.map((r) => r.turn.turnId)).toEqual(['t1', 't2', 't3']);
    }
    expect(last).toMatchObject({ kind: 'turn', index: 3 });
  });

  test('a turn with a diff, a reverted turn and a turn with an enabled revert break a run', () => {
    const withDiff = turn('d', ok('s'), ok('e'), CHANGED);
    const reverted = turn('r', gone('background_writers'), gone('background_writers'), null, true);
    const startOk = turn('o', ok('s'), gone('background_writers'));
    expect(quietTurn(withDiff, rollbackCaps)).toBe(false);
    expect(quietTurn(reverted, rollbackCaps)).toBe(false);
    expect(quietTurn(startOk, rollbackCaps)).toBe(false);
    // Without a rollback no revert is offered, so a start snapshot does not break the run.
    expect(quietTurn(startOk, noRollbackCaps)).toBe(true);
    const list = [quiet('a'), quiet('b'), withDiff, quiet('c'), quiet('e'), reverted, quiet('f')];
    expect(foldTurns(list, rollbackCaps).every((e) => e.kind === 'turn')).toBe(true);
    const broken = [
      quiet('a'),
      quiet('b'),
      startOk,
      quiet('c'),
      quiet('e'),
      quiet('f'),
      quiet('z'),
    ];
    expect(foldTurns(broken, rollbackCaps).map((e) => e.kind)).toEqual([
      'turn',
      'turn',
      'turn',
      'fold',
      'turn',
    ]);
  });

  test('foldText: one cause, the shared "No snapshot", "No file changes", "No changes to show"', () => {
    const none = () => false;
    const same = [quiet('a'), quiet('b'), quiet('c')];
    expect(foldText(same, none, checkpointText)).toBe('No snapshot: background work was running');
    expect(foldText(same, (r) => r === 'background_writers', checkpointText)).toBe('No snapshot');
    const mixed = [quiet('a'), turn('b', gone('queued_input'), gone('queued_input')), quiet('c')];
    expect(foldText(mixed, none, checkpointText)).toBe('No snapshot');
    const noChange = [turn('a', ok('s'), ok('e')), turn('b', ok('s'), ok('e'))];
    expect(foldText(noChange, none, checkpointText)).toBe('No file changes');
    const both = [turn('a', ok('s'), ok('e')), quiet('b')];
    expect(foldText(both, none, checkpointText)).toBe('No changes to show');
  });

  test('the list renders "Turns 2–5" in a closed details with the rows inside', () => {
    const bg = gone('background_writers');
    const html = view(codex(), [
      finalized('t1'),
      finalized('t2', bg, bg),
      finalized('t3', bg, bg),
      finalized('t4', bg, bg),
      finalized('t5', bg, bg),
      finalized('t6', bg, bg),
    ]);
    expect(html).toContain(
      '<li class="fd-turn-fold" data-first="2" data-last="5"><details><summary><span class="fd-turn-name">Turns 2–5</span>',
    );
    expect(html).not.toContain('<details open');
    const from = html.indexOf('fd-turn-fold');
    const to = html.indexOf('</details>', from);
    const fold = html.slice(from, to);
    expect(fold).toContain('No snapshot: background work was running');
    expect(fold).toContain('Turn 3');
    expect(fold).not.toContain('Turn 6');
    expect(html.slice(to)).toContain('Turn 6');
    expect(html).toContain('<span class="fd-turn-name">Turn 1</span>');
  });

  test('a turn without a start snapshot says why in its row', () => {
    const html = view(codex(), [
      finalized('t1', gone('provider_initiated'), ok('e'), {
        files_changed: 0,
        insertions: 0,
        deletions: 0,
      }),
      finalized('t2'),
    ]);
    expect(html).toContain(
      '<span class="fd-turn-stat" title="provider_initiated">No snapshot: the agent started the turn on its own</span>',
    );
    expect(html).not.toContain('No file changes');
  });

  test('the recovery reason codes have words', () => {
    expect(checkpointText('checkpoint_missing')).toBe('No snapshot: the snapshot is gone');
    expect(checkpointText('checkpoint_changed')).toBe('No snapshot: the snapshot was changed');
  });
});
