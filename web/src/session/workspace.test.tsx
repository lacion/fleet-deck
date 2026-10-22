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
import { type PaneTab, SessionView } from './SessionView';
import { DiffFlight, diffOpen, SinceNotes } from './TurnsPanel';
import {
  artifactFiles,
  fileActions,
  finalizedTurns,
  joinPath,
  mergeDiffPaths,
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
      'No start checkpoint: queued_input.',
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
    expect(html).toContain(
      '<span class="fd-turn-stat">no completion checkpoint: queued_input</span>',
    );
    expect(html).toContain('Show the diff');
    expect(html).toMatch(/<button type="button" class="fd-sm">Revert to here<\/button>/);
    expect(html).not.toContain('cannot roll back its conversation, so the board');
    expect(html).not.toContain('aria-label="Files"');
    const files = view(codex(), [finalized('t1')], undefined, 'files');
    expect(files).toContain('aria-label="Files"');
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
    expect(html).toContain(
      'This provider cannot roll back its conversation, so the board offers no revert.',
    );
    expect(html).not.toContain('file-only restore');
    expect(html).not.toContain('Revert to here');
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

  test('with a base the Changes tab opens "Since spawn", also before the first turn', () => {
    const html = view(codex({ base }), []);
    expect(html).toContain('aria-label="Changes view"');
    expect(html).toMatch(/aria-pressed="true"[^>]*>Since spawn</);
    expect(html).toMatch(/aria-pressed="false"[^>]*>By turn</);
    expect(html).toContain('Since the session started');
    expect(html).toContain('<code>main</code> <code>01234567</code>');
    expect(html).toContain('Loading the diff');
  });

  test('a guessed base opens "By turn" and without a base there is no toggle', () => {
    const guessed = view(
      codex({ base: { git_ref: 'origin/HEAD', oid: base.oid, inferred: true } }),
      [finalized('t1')],
    );
    expect(guessed).toContain('aria-label="Changes view"');
    expect(guessed).toMatch(/aria-pressed="false"[^>]*>Since spawn</);
    expect(guessed).toMatch(/aria-pressed="true"[^>]*>By turn</);
    expect(guessed).toContain('Show the diff');
    expect(guessed).not.toContain('Since the session started');
    const plain = view(codex(), [finalized('t1')]);
    // DL-191: the owner has the Branches view, so the toggle shows without a base.
    expect(plain).toContain('Changes view');
    expect(plain).not.toContain('Since spawn');
    expect(plain).toMatch(/aria-pressed="true"[^>]*>By turn</);
    expect(plain).toMatch(/aria-pressed="false"[^>]*>Branches</);
    expect(plain).toContain('Show the diff');
  });

  test('the "Since spawn" view says when the start commit was guessed', () => {
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
