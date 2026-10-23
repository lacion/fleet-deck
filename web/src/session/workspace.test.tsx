import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { CheckpointSide, DeckSession, DiffSummary, Event, SessionRevert } from '../gen';
import type { Timeline } from '../runtime/state';
import { claudeCaps, codexCaps } from '../testing/capabilities';
import { ownerEnv, PeopleProvider } from '../sharing/context';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import { envelopes } from '../testing/timeline';
import { sessionActions } from './commands';
import { checkpointText, hasSnapshot, outsideGit } from './checkpoint';
import { type PaneTab, SessionView } from './SessionView';
import { DiffFlight, diffOpen, rollbackNote, SinceNotes, SinceTotal } from './TurnsPanel';
import {
  artifactFiles,
  fileActions,
  finalizedTurns,
  joinPath,
  mergeDiffPaths,
  mergeTurnPaths,
  replaceDiffPaths,
  parentPath,
  revertBlock,
  revertText,
  setDiffTotal,
  useDiffPaths,
  useDiffTotal,
  workspaceFiles,
} from './workspace';

const key = { station_id: 'station-a', session_id: 's1' };
function Total({ id }: { readonly id: string }) {
  const total = useDiffTotal(id);
  return (
    <b>
      {total === undefined
        ? 'none'
        : `${total.files_changed}/${total.insertions}/${total.deletions}`}
    </b>
  );
}
const totalOf = (id: string) => renderToStaticMarkup(<Total id={id} />);
const ok = (commit: string): CheckpointSide => ({ state: 'available', commit });
const gone = (reason: string): CheckpointSide => ({ state: 'unavailable', reason });

function finalized(
  turn: string,
  start = ok(`${turn}-start`),
  end = ok(`${turn}-end`),
  diff: DiffSummary | null = { files_changed: 1, insertions: 2, deletions: 1 },
): Event {
  return {
    kind: 'turn.finalized',
    turn_id: turn,
    checkpoint: { worktree_id: 'wt', scope: 'tracked', start, end },
    diff_summary: end.state === 'available' ? diff : null,
  };
}

function reverted(turn: string, state: 'succeeded' | 'reconciled' | 'failed'): Event {
  return {
    kind: 'revert.updated',
    revert_id: 'rv-1',
    turn_id: turn,
    turns: 1,
    state,
    reason: null,
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

const view = (
  s: DeckSession,
  events: Event[],
  scopes = ['read', 'drive', 'approve'],
  tab: PaneTab = 'changes',
  layout: 'desktop' | 'tablet' = 'tablet',
) =>
  renderToStaticMarkup(
    <SessionView
      layout={layout}
      session={s}
      stations={[station()]}
      freshness="live"
      timeline={timeline(events)}
      actions={sessionActions(
        async () => ({}),
        key,
        scopes,
        undefined,
        async () => ({}),
      )}
      tab={tab}
    />,
  );

const recon: SessionRevert = {
  revert_id: 'rv-1',
  turn_id: 't2',
  state: 'reconciliation_required',
  reason: 'workspace_changed',
};

describe('turns and reverts (DL-097)', () => {
  test('finalized turns apply a successful revert to the target and the later turns', () => {
    const events = [finalized('t1'), finalized('t2'), finalized('t3'), reverted('t2', 'succeeded')];
    expect(finalizedTurns(envelopes('s1', events)).map((t) => [t.turnId, t.reverted])).toEqual([
      ['t1', false],
      ['t2', true],
      ['t3', true],
    ]);
    // A failed revert changes nothing; a reconciliation removes every turn.
    const failed = finalizedTurns(envelopes('s1', [finalized('t1'), reverted('t1', 'failed')]));
    expect(failed[0]?.reverted).toBe(false);
    const all = finalizedTurns(
      envelopes('s1', [finalized('t1'), finalized('t2'), reverted('t2', 'reconciled')]),
    );
    expect(all.every((t) => t.reverted)).toBe(true);
  });

  test('a revert is offered only with rollback, a start checkpoint and a quiet session', () => {
    const [turn] = finalizedTurns(envelopes('s1', [finalized('t1')]));
    if (!turn) throw new Error('no turn');
    const quiet = codex().summary;
    expect(revertBlock(turn, quiet, codexCaps())).toBeNull();
    expect(revertBlock(turn, quiet, claudeCaps())).toBe(
      'The provider cannot roll back its conversation.',
    );
    expect(revertBlock(turn, quiet, null)).toBe('The provider cannot roll back its conversation.');
    expect(revertBlock({ ...turn, reverted: true }, quiet, codexCaps())).toBe(
      'The turn is reverted.',
    );
    expect(revertBlock({ ...turn, start: gone('queued_input') }, quiet, codexCaps())).toBe(
      'No snapshot at the start of this turn: a queued message started next.',
    );
    expect(revertBlock({ ...turn, start: gone('submodule') }, quiet, codexCaps())).toBe(
      'No snapshot at the start of this turn.',
    );
    expect(revertBlock(turn, { ...quiet, revert: recon }, codexCaps())).toBe(
      'Another revert holds the session.',
    );
    expect(revertBlock(turn, { ...quiet, current_turn: 't9' }, codexCaps())).toBe('A turn runs.');
    expect(revertText('reconciliation_required', 'rollback_uncertain')).toContain(
      'Reason: rollback_uncertain.',
    );
  });

  test('a Codex session shows its turns with a diff and a revert control', () => {
    const html = view(codex(), [finalized('t1'), finalized('t2', ok('s'), gone('queued_input'))]);
    expect(html).toContain('aria-label="Turns"');
    expect(html).toMatch(
      /data-turn="t1" data-reverted="false"><span class="fd-turn-name">Turn 1<\/span><span class="fd-turn-stat"><span class="fd-add">\+2<\/span><span class="fd-del">−1<\/span><span class="fd-turn-files">1 file<\/span><\/span>/,
    );
    expect(html).toContain('<span class="fd-turn-name">Turn 2</span>');
    // The reason in plain words; the station code stays in the title.
    expect(html).toContain(
      '<span class="fd-turn-stat" title="queued_input">No snapshot: a queued message started next</span>',
    );
    expect(html).not.toContain('data-note="no-snapshots"');
    expect(html).toContain('Show the diff');
    expect(html).toMatch(/<button type="button" class="fd-sm">Revert to here<\/button>/);
    expect(html).not.toContain('cannot roll back its conversation, so the board');
    expect(html).not.toContain('aria-label="Files"');
    const files = view(codex(), [finalized('t1')], undefined, 'files');
    expect(files).toContain('aria-label="Files"');
  });

  test('a folder outside Git: one empty state, no rows, no notes and no Branches view', () => {
    const outside = gone('not_a_git_worktree');
    const turns = [
      finalized('t1', outside, gone('background_writers')),
      finalized('t2', outside, outside),
    ];
    const html = view(codex(), turns);
    // §4.7: a 20px --faint icon, a title and one sentence, with no box (ui/page EmptyState).
    expect(html).toContain('<div class="fd-emptystate"><svg');
    expect(html).toContain('lucide-file-diff');
    expect(html).toContain('<p class="fd-emptystate-title">No changes to show</p>');
    expect(html).toContain(
      '<p class="fd-emptystate-body">This folder is not a Git repository, so the board keeps no file snapshots for this session.</p>',
    );
    // No list of dead rows, no notes, and no Branches segment for a folder that has no Git.
    expect(html).not.toContain('No snapshot');
    expect(html).not.toContain('data-note="no-snapshots"');
    expect(html).not.toContain('cannot be rolled back');
    expect(html).not.toContain('Branches');
    expect(html).not.toContain('aria-label="Changes view"');
    // One turn outside Git is the same state.
    expect(view(codex(), [finalized('t1', outside, outside)])).toContain('No changes to show');
    // Claude (no rollback) gets the same empty state, with no rollback note.
    const claude: DeckSession = {
      station_id: 'station-a',
      status: 'idle',
      summary: summary('s1', 'idle', 1, { capabilities: claudeCaps() }),
    };
    const claudeHtml = view(claude, turns);
    expect(claudeHtml).toContain('No changes to show');
    expect(claudeHtml).not.toContain('cannot be rolled back');
  });

  test('turns without a snapshot at their end: the reason once above the list of revert rows', () => {
    const outside = gone('not_a_git_worktree');
    // Background work on two of three turns: that reason once; the other row keeps its own words.
    // The starts have snapshots, so each row still offers "Revert to here": the list stays.
    const busy = view(codex(), [
      finalized('t1', ok('a'), gone('background_writers')),
      finalized('t2', ok('b'), gone('queued_input')),
      finalized('t3', ok('c'), gone('background_writers')),
    ]);
    expect(busy).toContain(
      'The board kept no file snapshots for these turns: background work was running.',
    );
    expect(busy).toContain(
      'title="queued_input">No snapshot: a queued message started next</span>',
    );
    expect(busy).toContain('Revert to here');
    expect(busy).not.toContain('fd-emptystate');
    // One turn, or a turn with changes: no note.
    expect(view(codex(), [finalized('t1', ok('a'), outside)])).not.toContain('no-snapshots');
    expect(checkpointText('submodule')).toBe('No snapshot of the files for this turn');
  });

  test('turns that all lost their snapshots: one empty state, not a list of "No snapshot" rows', () => {
    const busy = gone('background_writers');
    const turns = [
      finalized('t1', busy, busy),
      finalized('t2', busy, gone('queued_input')),
      finalized('t3', busy, busy),
    ];
    const html = view(codex(), turns);
    // §4.7, as a folder outside Git: the icon, a title and why, with no box and no rows.
    expect(html).toContain('<div class="fd-emptystate"><svg');
    expect(html).toContain('lucide-file-diff');
    expect(html).toContain('<p class="fd-emptystate-title">No changes to show</p>');
    expect(html).toContain(
      '<p class="fd-emptystate-body">The board kept no file snapshots for the 3 finished turns: background work was running.</p>',
    );
    expect(html).not.toContain('No snapshot');
    expect(html).not.toContain('Turn 1');
    expect(html).not.toContain('Revert to here');
    expect(html).not.toContain('data-note="no-snapshots"');
    // Claude cannot roll back: start snapshots offer no revert either, so the state is the same.
    const claude: DeckSession = {
      station_id: 'station-a',
      status: 'idle',
      summary: summary('s1', 'idle', 1, { capabilities: claudeCaps() }),
    };
    const started = [finalized('t1', ok('a'), busy), finalized('t2', ok('b'), busy)];
    const claudeHtml = view(claude, started);
    expect(claudeHtml).toContain('No changes to show');
    expect(claudeHtml).toContain('for the 2 finished turns: background work was running.');
    expect(claudeHtml).not.toContain('cannot be rolled back');
    // A turn with changes keeps the list.
    const mixed = view(codex(), [
      finalized('t1', busy, busy),
      finalized('t2', ok('s'), ok('e'), { files_changed: 1, insertions: 2, deletions: 0 }),
    ]);
    expect(mixed).not.toContain('fd-emptystate');
    expect(mixed).toContain('Show the diff');
  });

  test('the diff button: a loaded diff hides; nothing or a failed load shows (and retries)', () => {
    const loaded = {
      diff: { turn_id: 't1', patch: '', truncated: false } as never,
      snapshot: null,
      fellBack: false,
    };
    expect(diffOpen(undefined)).toBe(false);
    expect(diffOpen('The diff request failed.')).toBe(false);
    expect(diffOpen(loaded)).toBe(true);
  });

  test('the diff toggle: Show, then Hide before the answer keeps the diff closed', () => {
    const flight = new DiffFlight();
    // Show: one request starts; the button says "Loading…" (aria-busy).
    expect(flight.start('t1')).toBe(true);
    expect(flight.loading('t1')).toBe(true);
    // A second Show press during the load starts no second request.
    expect(flight.start('t1')).toBe(false);
    // Hide during the load: the button says "Show the diff" again, the late answer is dropped.
    flight.hide('t1');
    expect(flight.loading('t1')).toBe(false);
    expect(flight.finish('t1')).toBe(false);
    // The next Show loads again, and its answer shows.
    expect(flight.start('t1')).toBe(true);
    expect(flight.finish('t1')).toBe(true);
    // Show, Hide, Show during one load: the running load shows its answer.
    expect(flight.start('t2')).toBe(true);
    flight.hide('t2');
    expect(flight.start('t2')).toBe(false);
    expect(flight.loading('t2')).toBe(true);
    expect(flight.finish('t2')).toBe(true);
    // A Hide of a loaded diff (no load runs) leaves no mark on the next load.
    flight.hide('t3');
    expect(flight.start('t3')).toBe(true);
    expect(flight.finish('t3')).toBe(true);
  });

  test('a turn without file changes says so and has no Show the diff', () => {
    const html = view(codex(), [
      finalized('t1', ok('s'), ok('e'), { files_changed: 0, insertions: 0, deletions: 0 }),
    ]);
    expect(html).toContain('<span class="fd-turn-name">Turn 1</span>');
    expect(html).toContain('<span class="fd-turn-stat">No file changes</span>');
    expect(html).not.toContain('Show the diff');
    expect(html).not.toContain('+0');
  });

  test('a turn with files and no line counts shows the files only', () => {
    const html = view(codex(), [
      finalized('t1', ok('s'), ok('e'), { files_changed: 2, insertions: 0, deletions: 0 }),
    ]);
    expect(html).toContain(
      '<span class="fd-turn-stat"><span class="fd-turn-files">2 files</span></span>',
    );
    expect(html).toContain('Show the diff');
  });

  test('a reverted turn says so', () => {
    const html = view(codex(), [finalized('t1'), reverted('t1', 'succeeded')]);
    expect(html).toContain('data-turn="t1" data-reverted="true"');
    expect(html).toContain('<span class="fd-turn-stat">reverted</span>');
  });

  test('a Claude session gets no revert control and says why', () => {
    const s: DeckSession = {
      station_id: 'station-a',
      status: 'idle',
      summary: summary('s1', 'idle', 1, { capabilities: claudeCaps() }),
    };
    const html = view(s, [finalized('t1')]);
    // The note names the provider.
    expect(html).toContain('Claude sessions cannot be rolled back, so the board offers no revert.');
    expect(html).not.toContain('file-only restore');
    expect(html).not.toContain('Revert to here');
    expect(rollbackNote(null)).toBe(
      'This session cannot be rolled back, so the board offers no revert.',
    );
    // Without any snapshot no revert is possible anyway: no note (one empty state says why).
    const none = view(s, [
      finalized('t1', gone('background_writers'), gone('background_writers')),
      finalized('t2', gone('background_writers'), gone('background_writers')),
    ]);
    expect(none).not.toContain('cannot be rolled back');
    expect(none).toContain('<p class="fd-emptystate-title">No changes to show</p>');
  });

  test('read-only scopes and a running turn disable the revert; a read-only board has no files', () => {
    const readOnly = view(codex(), [finalized('t1')], ['read']);
    expect(readOnly).toMatch(
      /<button type="button" class="fd-sm" disabled="">Revert to here<\/button>/,
    );
    const files = view(codex(), [finalized('t1')], ['read'], 'files');
    expect(files).not.toContain('aria-label="Files"');
    expect(files).toContain(
      'Files are available when the board is live and you have the drive scope.',
    );
    const running = view(codex({ current_turn: 't2', activity: 'working' }), [finalized('t1')]);
    expect(running).toMatch(/disabled="" title="A turn runs.">Revert to here/);
  });

  test('a revert that needs reconciliation shows the owner actions', () => {
    const html = view(codex({ revert: recon }), [finalized('t1'), finalized('t2')]);
    expect(html).toContain('data-revert-state="reconciliation_required"');
    expect(html).toContain('role="alert">The revert needs reconciliation');
    expect(html).toContain('Reason: workspace_changed.');
    expect(html).toMatch(/<button type="button" class="fd-sm">Retry the file restore<\/button>/);
    expect(html).toMatch(/<button type="button" class="fd-sm">Mark as reconciled<\/button>/);
    expect(html).toContain('A revert holds the session; no new revert starts.');
    const readOnly = view(codex({ revert: recon }), [finalized('t1')], ['read']);
    expect(readOnly).toMatch(
      /<button type="button" class="fd-sm" disabled="">Mark as reconciled<\/button>/,
    );
    const done = view(codex({ revert: { ...recon, state: 'succeeded', reason: null } }), []);
    expect(done).toContain('role="status">The revert succeeded');
    expect(done).not.toContain('Mark as reconciled');
  });
});

describe('the diff since the session started (R1.2)', () => {
  const base = {
    git_ref: 'main',
    oid: '0123456789abcdef0123456789abcdef01234567',
    inferred: false,
  };

  test('with a base the Changes tab opens "Whole session", also before the first turn', () => {
    const html = view(codex({ base }), []);
    expect(html).toContain('aria-label="Changes view"');
    expect(html).toMatch(/aria-pressed="true"[^>]*>Whole session</);
    expect(html).toMatch(/aria-pressed="false"[^>]*>By turn</);
    // One head row: where the changes count from (the totals come first once the diff loaded),
    // then the Refresh icon button at the end of the row.
    expect(html).toContain(
      '<div class="fd-since-head"><span class="fd-since-base">Changes since <code>main</code> at <code>01234567</code></span><button type="button" class="fd-iconbtn fd-sm" aria-label="Refresh"',
    );
    expect(html).toContain('Loading the diff');
    // No base ref: the commit only.
    const noRef = view(codex({ base: { oid: base.oid, inferred: false } }), []);
    expect(noRef).toContain(
      '<span class="fd-since-base">Changes since <code>01234567</code></span>',
    );
  });

  test('the head row puts the totals first: `+1,286 −400 · 3 files`', () => {
    const html = renderToStaticMarkup(
      <SinceTotal stat={{ files_changed: 3, insertions: 1286, deletions: 400 }} />,
    );
    expect(html).toBe(
      '<span class="fd-since-total"><span class="fd-add">+1,286</span><span class="fd-del">−400</span><span class="fd-since-files">· 3 files</span></span>',
    );
    // Files without line counts: no leading dot.
    expect(
      renderToStaticMarkup(<SinceTotal stat={{ files_changed: 1, insertions: 0, deletions: 0 }} />),
    ).toContain('<span class="fd-since-files">1 file</span>');
  });

  test('outsideGit: a turn says the folder is outside Git and no turn has a snapshot', () => {
    const turn = (start: CheckpointSide | null, end: CheckpointSide | null) => ({
      turnId: 't',
      start,
      end,
      diff: null,
      reverted: false,
    });
    const outside = gone('not_a_git_worktree');
    expect(outsideGit([turn(outside, outside)])).toBe(true);
    expect(outsideGit([turn(null, gone('background_writers')), turn(outside, outside)])).toBe(true);
    expect(outsideGit([turn(ok('a'), outside)])).toBe(false);
    expect(outsideGit([turn(gone('queued_input'), gone('queued_input'))])).toBe(false);
    expect(outsideGit([])).toBe(false);
    expect(hasSnapshot([turn(outside, outside)])).toBe(false);
    expect(hasSnapshot([turn(ok('a'), outside)])).toBe(true);
  });

  test('a guessed base opens "By turn" and without a base there is no toggle', () => {
    const guessed = view(
      codex({ base: { git_ref: 'origin/HEAD', oid: base.oid, inferred: true } }),
      [finalized('t1')],
    );
    expect(guessed).toContain('aria-label="Changes view"');
    expect(guessed).toMatch(/aria-pressed="false"[^>]*>Whole session</);
    expect(guessed).toMatch(/aria-pressed="true"[^>]*>By turn</);
    expect(guessed).toContain('Show the diff');
    expect(guessed).not.toContain('fd-since-head');
    const plain = view(codex(), [finalized('t1')]);
    // DL-191: the owner has the Branches view, so the toggle shows without a base.
    expect(plain).toContain('Changes view');
    expect(plain).not.toContain('Whole session');
    expect(plain).toMatch(/aria-pressed="true"[^>]*>By turn</);
    expect(plain).toMatch(/aria-pressed="false"[^>]*>Branches</);
    expect(plain).toContain('Show the diff');
  });

  test('the "Whole session" view says when the start commit was guessed', () => {
    const html = renderToStaticMarkup(
      <SinceNotes
        base={{ git_ref: 'origin/HEAD', oid: base.oid, inferred: true }}
        scope="worktree"
      />,
    );
    expect(html).toContain(
      'The start commit was guessed (origin/HEAD), because this session started before Fleet Deck recorded it.',
    );
    expect(html).not.toContain('shared folder');
    const noRef = renderToStaticMarkup(
      <SinceNotes base={{ oid: base.oid, inferred: true }} scope={undefined} />,
    );
    expect(noRef).toContain('The start commit was guessed (a commit),');
    const recorded = renderToStaticMarkup(<SinceNotes base={base} scope="worktree" />);
    expect(recorded).toBe('');
  });

  test('a session in a shared folder says that the diff shows only its own files', () => {
    const html = renderToStaticMarkup(<SinceNotes base={base} scope="session_files" />);
    expect(html).toContain(
      'This session works in a shared folder. The diff shows only the files that this session changed.',
    );
    expect(html).not.toContain('guessed');
    expect(renderToStaticMarkup(<SinceNotes base={base} scope={undefined} />)).toBe('');
  });

  test('the session diff uses the session path', async () => {
    const asked: string[] = [];
    const diff = {
      summary: { files_changed: 0, insertions: 0, deletions: 0 },
      patch: '',
      truncated: false,
    };
    const files = fileActions(async (path) => {
      asked.push(path);
      return diff;
    }, key);
    await files.sessionDiff();
    expect(asked).toEqual(['/api/stations/station-a/sessions/s1/diff']);
    await expect(workspaceFiles(async () => ({}), 'station-a', '/w').sessionDiff()).rejects.toThrow(
      'A folder has no session diff.',
    );
  });

  test('the card total is kept per session', () => {
    setDiffTotal('station-a/s1', { files_changed: 2, insertions: 5, deletions: 1 });
    expect(totalOf('station-a/s1')).toBe('<b>2/5/1</b>');
    expect(totalOf('station-a/other')).toBe('<b>none</b>');
  });

  test('a turn diff or the Files list adds its paths; the session diff replaces them', () => {
    const pathsOf = (ref: string) => {
      function Probe() {
        return <b>{useDiffPaths(ref)?.join(',') ?? 'none'}</b>;
      }
      return renderToStaticMarkup(<Probe />);
    };
    mergeDiffPaths('station-a/m1', ['b.ts', 'a.ts', 'b.ts']);
    expect(pathsOf('station-a/m1')).toBe('<b>b.ts,a.ts</b>');
    mergeDiffPaths('station-a/m1', ['a.ts', 'c.ts']);
    expect(pathsOf('station-a/m1')).toBe('<b>b.ts,a.ts,c.ts</b>');
    setDiffTotal('station-a/m1', { files_changed: 1, insertions: 1, deletions: 0 }, ['c.ts']);
    expect(pathsOf('station-a/m1')).toBe('<b>c.ts</b>');
  });

  test('a full session diff replaces the paths; an empty one empties them; turns then add nothing', () => {
    const pathsOf = (ref: string) => {
      function Probe() {
        const paths = useDiffPaths(ref);
        return <b>{paths === undefined ? 'none' : `[${paths.join(',')}]`}</b>;
      }
      return renderToStaticMarkup(<Probe />);
    };
    // A turn listed `restored.ts`; a later turn restored it, so the full diff no longer has it.
    mergeDiffPaths('station-a/f1', ['restored.ts', 'kept.ts']);
    expect(pathsOf('station-a/f1')).toBe('<b>[restored.ts,kept.ts]</b>');
    replaceDiffPaths('station-a/f1', ['kept.ts']);
    expect(pathsOf('station-a/f1')).toBe('<b>[kept.ts]</b>');
    mergeDiffPaths('station-a/f1', ['restored.ts']);
    expect(pathsOf('station-a/f1')).toBe('<b>[kept.ts]</b>');
    // A clean diff is an empty list, not "unknown".
    replaceDiffPaths('station-a/f1', []);
    expect(pathsOf('station-a/f1')).toBe('<b>[]</b>');
    mergeTurnPaths('station-a/f1', undefined, ['again.ts']);
    expect(pathsOf('station-a/f1')).toBe('<b>[]</b>');
  });

  test('the diff of a reverted turn adds no paths; a kept turn adds its paths', () => {
    const pathsOf = (ref: string) => {
      function Probe() {
        return <b>{useDiffPaths(ref)?.join(',') ?? 'none'}</b>;
      }
      return renderToStaticMarkup(<Probe />);
    };
    const turn = (reverted: boolean) => ({
      turnId: reverted ? 't1' : 't2',
      start: ok('a1'),
      end: ok('b1'),
      diff: { files_changed: 1, insertions: 1, deletions: 0 },
      reverted,
    });
    mergeTurnPaths('station-a/r1', turn(true), ['gone.ts']);
    expect(pathsOf('station-a/r1')).toBe('<b>none</b>');
    mergeTurnPaths('station-a/r1', turn(false), ['kept.ts']);
    expect(pathsOf('station-a/r1')).toBe('<b>kept.ts</b>');
    mergeTurnPaths('station-a/r1', undefined, ['other.ts']);
    expect(pathsOf('station-a/r1')).toBe('<b>kept.ts,other.ts</b>');
  });
});

describe('workspace actions', () => {
  test('the file service and the diff use the session paths', async () => {
    const asked: string[] = [];
    const answers: Record<string, unknown> = {
      list: { root: { session: 's1' }, path: 'src', entries: [], truncated: false },
      read: { path: 'a b.txt', size: 3, binary: false, text: 'one', truncated: false },
      search: { hits: [], truncated: false, backend: 'git' },
      diff: {
        summary: { files_changed: 0, insertions: 0, deletions: 0 },
        patch: '',
        truncated: false,
      },
    };
    const files = fileActions(async (path) => {
      asked.push(path);
      const kind = path.includes('/turns/') ? 'diff' : (path.match(/files\/(\w+)/)?.[1] ?? '');
      return answers[kind];
    }, key);
    await files.list('src');
    await files.read('a b.txt');
    await files.search('fn main', 'content');
    await files.search('src/ma', 'name', 20);
    await files.diff('turn/1');
    expect(asked).toEqual([
      '/api/stations/station-a/files/list?session=s1&path=src',
      '/api/stations/station-a/files/read?session=s1&path=a%20b.txt',
      '/api/stations/station-a/files/search?session=s1&q=fn%20main&mode=content',
      '/api/stations/station-a/files/search?session=s1&q=src%2Fma&mode=name&max_hits=20',
      '/api/stations/station-a/sessions/s1/turns/turn%2F1/diff',
    ]);
    const bad = fileActions(async () => ({ entries: [] }), key);
    await expect(bad.list('')).rejects.toThrow('The deck sent an invalid directory listing.');
    expect(parentPath('a/b/c')).toBe('a/b');
    expect(parentPath('a')).toBe('');
    expect(joinPath('', 'a')).toBe('a');
    expect(joinPath('a', 'b')).toBe('a/b');
  });

  test('a folder of a station is read with workspace= and has no turns', async () => {
    const asked: string[] = [];
    const answers: Record<string, unknown> = {
      list: { path: '', entries: [], truncated: false },
      read: { path: 'a.txt', size: 1, binary: false, text: 'x', truncated: false },
      search: { hits: [], truncated: false, backend: 'walk' },
    };
    const files = workspaceFiles(
      async (path) => {
        asked.push(path);
        return answers[path.match(/files\/(\w+)/)?.[1] ?? ''];
      },
      'station-a',
      '/home/me/my app',
    );
    await files.list('src');
    await files.read('a b.txt');
    await files.search('fn main', 'name');
    expect(asked).toEqual([
      '/api/stations/station-a/files/list?workspace=%2Fhome%2Fme%2Fmy%20app&path=src',
      '/api/stations/station-a/files/read?workspace=%2Fhome%2Fme%2Fmy%20app&path=a%20b.txt',
      '/api/stations/station-a/files/search?workspace=%2Fhome%2Fme%2Fmy%20app&q=fn%20main&mode=name',
    ]);
    await expect(files.diff('t1')).rejects.toThrow('A folder has no turns.');
    expect(asked).toHaveLength(3);
  });

  test('revert and the two resolutions post one command each', async () => {
    const posted: [string, unknown][] = [];
    let n = 0;
    const actions = sessionActions(
      async (path, body) => {
        posted.push([path, body]);
        return { receipt: { state: 'accepted', reject: null } };
      },
      key,
      ['read', 'drive'],
      () => `cmd-${++n}`,
    );
    expect(actions.files).toBeNull();
    await actions.revert('t1');
    await actions.resolveRevert('rv-1', 'retry_restore');
    await actions.resolveRevert('rv-1', 'reconciled');
    expect(posted.map(([path]) => path)).toEqual([
      '/api/commands/session.revert',
      '/api/commands/session.resolve_revert',
      '/api/commands/session.resolve_revert',
    ]);
    expect(posted.map(([, body]) => (body as { command: unknown }).command)).toEqual([
      { op: 'revert', session_id: 's1', turn_id: 't1' },
      { op: 'resolve_revert', session_id: 's1', revert_id: 'rv-1', resolution: 'retry_restore' },
      { op: 'resolve_revert', session_id: 's1', revert_id: 'rv-1', resolution: 'reconciled' },
    ]);
  });
});

describe('the Branches view and Show its work, owner only (DL-191)', () => {
  const memberEnv = {
    ...ownerEnv,
    member: { role: 'collaborator' as const, name: 'Ann', scope: null },
  };
  const agent: Event = {
    kind: 'tool.started',
    node_id: 'agent-1',
    parent_node_id: null,
    tool_kind: 'subagent',
    title: 'Agent',
    input_summary: 'explore',
    files: [],
    provider_refs: { tool_use_id: 'toolu_01' } as never,
  };
  const render = (options: { member?: boolean; get?: boolean; tab?: PaneTab; events?: Event[] }) =>
    renderToStaticMarkup(
      <PeopleProvider value={options.member ? memberEnv : ownerEnv}>
        <SessionView
          layout="tablet"
          session={codex()}
          stations={[station()]}
          freshness="live"
          timeline={timeline(options.events ?? [])}
          actions={sessionActions(
            async () => ({}),
            key,
            ['read', 'drive', 'approve'],
            undefined,
            options.get === false ? null : async () => ({}),
          )}
          tab={options.tab ?? 'changes'}
        />
      </PeopleProvider>,
    );

  test('the toggle offers Branches only to the owner with a reader', () => {
    const owner = render({ events: [finalized('t1')] });
    expect(owner).toContain('Changes view');
    expect(owner).toMatch(/aria-pressed="false"[^>]*>Branches</);
    expect(render({ events: [finalized('t1')], member: true })).not.toContain('Branches');
    expect(render({ events: [finalized('t1')], get: false })).not.toContain('Branches');
  });

  test('an owner session with no turn, no base and no revert still reaches Branches', () => {
    const owner = render({});
    expect(owner).toContain('Changes view');
    expect(owner).toContain('No finished turns yet. Changes appear here when a turn ends.');
    expect(owner).toMatch(/aria-pressed="false"[^>]*>Branches</);
    const member = render({ member: true });
    expect(member).toContain('No finished turns yet. Changes appear here when a turn ends.');
    expect(member).not.toContain('Changes view');
  });

  test('the subagent card has Show its work for the owner and not for a member', () => {
    const events = [agent];
    expect(render({ tab: 'chat', events })).toContain('Show its work');
    expect(render({ tab: 'chat', events, member: true })).not.toContain('Show its work');
    expect(render({ tab: 'chat', events, get: false })).not.toContain('Show its work');
  });
});

describe('artifact files (FG-12)', () => {
  test('the root query names the session in the artifacts part, and there are no turns', async () => {
    const asked: string[] = [];
    const files = artifactFiles(
      async (path) => {
        asked.push(path);
        return path.includes('/list')
          ? { path: '', entries: [], truncated: false }
          : { path: 'a.txt', size: 1, binary: false, text: 'x', truncated: false };
      },
      { station_id: 'station-a', session_id: 's 1/x' },
    );
    await files.list('out dir');
    await files.read('a b.txt');
    expect(asked).toEqual([
      '/api/stations/station-a/files/list?artifacts=s%201%2Fx&path=out%20dir',
      '/api/stations/station-a/files/read?artifacts=s%201%2Fx&path=a%20b.txt',
    ]);
    await expect(files.diff('t1')).rejects.toThrow('The artifact folder has no turns.');
    await expect(files.sessionDiff()).rejects.toThrow('no session diff');
  });
});
