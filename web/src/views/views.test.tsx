// R6 (DL-169): the unread mark, the focus line, hibernation and the board calls of the views.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { isHidden, sidebarModel } from '../app/sessions';
import { Sidebar } from '../app/Sidebar';
import { FleetBoard } from '../board/FleetBoard';
import type { DeckSession } from '../gen';
import { activityItem, activityPath, viewsActions } from '../runtime/views';
import type { Outcome, SessionActions } from '../session/commands';
import { SessionView } from '../session/SessionView';
import { sessionActions } from '../session/commands';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { envelopes } from '../testing/timeline';
import { isHibernated, RESUME_TRIES, resumeThenSend } from './hibernate';
import { FocusLine, UnreadMark, unreadOf } from './marks';
import { topSeq } from './markRead';

const NOW = 1_760_000_040_000;

const withUnread = (c: DeckSession, unread: number): DeckSession => ({ ...c, unread });

describe('unread marks', () => {
  test('a count shows in a pill, nine and below as is, above as 9+, zero not at all', () => {
    expect(renderToStaticMarkup(<UnreadMark count={3} />)).toContain('>3</span>');
    expect(renderToStaticMarkup(<UnreadMark count={3} />)).toContain('aria-label="3 unread"');
    expect(renderToStaticMarkup(<UnreadMark count={9} />)).toContain('>9</span>');
    expect(renderToStaticMarkup(<UnreadMark count={10} />)).toContain('>9+</span>');
    expect(renderToStaticMarkup(<UnreadMark count={0} />)).toBe('');
    expect(unreadOf(card('a'))).toBe(0);
    expect(unreadOf(withUnread(card('a'), 4))).toBe(4);
  });

  test('the fleet card and the session row both show the count', () => {
    const sessions = [withUnread(card('u1', 'idle', 'station-a', { title: 'Docs' }), 2)];
    const board = renderToStaticMarkup(
      <FleetBoard sessions={sessions} stations={[station()]} freshness="live" now={NOW} />,
    );
    expect(board).toContain('class="fd-unread"');
    expect(board).toContain('aria-label="2 unread"');
    const list = renderToStaticMarkup(
      <Sidebar
        sessions={sessions}
        stations={[station()]}
        route={{ name: 'home' }}
        terminalCount={0}
        onNavigate={() => {}}
        onOpenTerminals={() => {}}
        now={NOW}
      />,
    );
    expect(list).toContain('aria-label="2 unread"');
    const none = renderToStaticMarkup(
      <FleetBoard sessions={[card('u2')]} stations={[station()]} freshness="live" now={NOW} />,
    );
    expect(none).not.toContain('fd-unread');
  });

  test('the cursor is the highest session_seq of the timeline', () => {
    expect(topSeq([])).toBe(0);
    expect(
      topSeq(
        envelopes('s', [
          { kind: 'session.notice', code: 'a', text: 'a' },
          { kind: 'session.notice', code: 'b', text: 'b' },
          { kind: 'session.notice', code: 'c', text: 'c' },
        ]),
      ),
    ).toBe(3);
  });
});

describe('the focus line (R6.4)', () => {
  test('a card, a row and the line itself show the focus as one muted line', () => {
    const sessions = [
      card('f1', 'working', 'station-a', { title: 'Docs', focus: 'writing the changelog' }),
    ];
    const board = renderToStaticMarkup(
      <FleetBoard sessions={sessions} stations={[station()]} freshness="live" now={NOW} />,
    );
    expect(board).toContain(
      '<span class="fd-focus" title="writing the changelog">writing the changelog</span>',
    );
    const row = renderToStaticMarkup(
      <Sidebar
        sessions={sessions}
        stations={[station()]}
        route={{ name: 'home' }}
        terminalCount={0}
        onNavigate={() => {}}
        onOpenTerminals={() => {}}
        now={NOW}
      />,
    );
    expect(row).toContain('class="fd-focus"');
  });

  test('without a focus nothing shows', () => {
    const c = card('f2');
    expect(renderToStaticMarkup(<FocusLine summary={c.summary} />)).toBe('');
    expect(renderToStaticMarkup(<FocusLine summary={{ ...c.summary, focus: '  ' }} />)).toBe('');
    const board = renderToStaticMarkup(
      <FleetBoard sessions={[c]} stations={[station()]} freshness="live" now={NOW} />,
    );
    expect(board).not.toContain('fd-focus');
  });
});

describe('hibernation (R6.6)', () => {
  const hibernated = card('h1', 'ended', 'station-a', {
    title: 'Docs',
    state: 'exited',
    hibernated: true,
  });

  test('an exited session with the flag is hibernated; a plain exit or a running one is not', () => {
    expect(isHibernated(hibernated)).toBe(true);
    expect(isHibernated(card('e', 'ended', 'station-a', { state: 'exited' }))).toBe(false);
    expect(isHibernated(card('r', 'working', 'station-a', { hibernated: true }))).toBe(false);
  });

  test('the card and the row say Hibernated', () => {
    const board = renderToStaticMarkup(
      <FleetBoard sessions={[hibernated]} stations={[station()]} freshness="live" now={NOW} />,
    );
    expect(board).toContain('data-note="hibernated">Hibernated · resumes with your next prompt');
    const row = renderToStaticMarkup(
      <Sidebar
        sessions={[hibernated]}
        stations={[station()]}
        route={{ name: 'home' }}
        terminalCount={0}
        onNavigate={() => {}}
        onOpenTerminals={() => {}}
        now={NOW}
      />,
    );
    expect(row).toContain('Hibernated: ');
  });

  test('a hibernated session lists with the live ones and "Clear" cannot hide it', () => {
    const plain = card('p1', 'ended', 'station-a', { state: 'exited' });
    const model = sidebarModel([hibernated, plain], [station()], '');
    expect(model.groups[0]?.live.map((c) => c.summary.session_id)).toEqual(['h1']);
    expect(model.groups[0]?.ended.map((c) => c.summary.session_id)).toEqual(['p1']);
    const all = new Set(['station-a/h1', 'station-a/p1']);
    expect(isHidden(hibernated, all)).toBe(false);
    expect(isHidden(plain, all)).toBe(true);
  });

  const view = (c: DeckSession) =>
    renderToStaticMarkup(
      <SessionView
        session={c}
        stations={[station()]}
        freshness="live"
        timeline={undefined}
        actions={sessionActions(
          async () => ({}),
          { station_id: 'station-a', session_id: c.summary.session_id },
          ['read', 'drive'],
        )}
        shells={undefined}
        terminals={undefined}
        watch={undefined}
        layout="desktop"
        tab="chat"
      />,
    );

  test('the session page takes a prompt for a hibernated session and an ended one does not', () => {
    const caps = claudeCaps();
    const sleeping = card('h2', 'ended', 'station-a', {
      state: 'exited',
      hibernated: true,
      capabilities: caps,
    });
    const html = view(sleeping);
    expect(html).toContain('>Hibernated</span>');
    expect(html).not.toMatch(/<textarea id="fd-composer-text" rows="1" disabled=""/);
    expect(html).not.toContain('The session ended. Resume it to send prompts.');
    // an ended session that can resume has a Resume bar in place of the composer
    const ended = view(card('e2', 'ended', 'station-a', { state: 'exited', capabilities: caps }));
    expect(ended).not.toContain('<textarea');
    expect(ended).toContain('class="fd-composer-resume"');
    expect(ended).toContain('The session ended. Resume it to send prompts.');
    expect(ended).toContain('Resume</button>');
  });

  const fakeActions = (sends: Outcome[], resumed: Outcome = { ok: true, text: 'Done.' }) => {
    const calls: string[] = [];
    const actions = {
      resume: async () => {
        calls.push('resume');
        return resumed;
      },
      send: async (kind: string, text: string) => {
        calls.push(`send:${kind}:${text}`);
        return sends.shift() ?? { ok: false, text: 'Refused: out of answers' };
      },
    } as unknown as SessionActions;
    return { actions, calls };
  };
  const noWait = async () => {};

  test('a send resumes first, then sends the prompt until the station takes it', async () => {
    const { actions, calls } = fakeActions([
      { ok: false, text: 'Refused: the session has ended' },
      { ok: false, text: 'Refused: the session has ended' },
      { ok: true, text: 'Done.' },
    ]);
    const out = await resumeThenSend(actions, 'prompt', 'continue', [], noWait);
    expect(out).toEqual({ ok: true, text: 'Done.' });
    expect(calls).toEqual([
      'resume',
      'send:prompt:continue',
      'send:prompt:continue',
      'send:prompt:continue',
    ]);
  });

  test('a refused resume sends nothing', async () => {
    const { actions, calls } = fakeActions([], { ok: false, text: 'Refused: no slot' });
    const out = await resumeThenSend(actions, 'prompt', 'x', [], noWait);
    expect(out).toEqual({ ok: false, text: 'Not resumed: Refused: no slot' });
    expect(calls).toEqual(['resume']);
  });

  test('a failure that is not a refusal ends the loop at once; refusals stop at the limit', async () => {
    const lost = fakeActions([{ ok: false, text: 'Not delivered: link down' }]);
    expect((await resumeThenSend(lost.actions, 'prompt', 'x', [], noWait)).text).toBe(
      'Not delivered: link down',
    );
    expect(lost.calls).toEqual(['resume', 'send:prompt:x']);
    const stuck = fakeActions([]);
    const out = await resumeThenSend(stuck.actions, 'prompt', 'x', [], noWait);
    expect(out.ok).toBe(false);
    expect(out.text).toContain('did not take the prompt');
    expect(stuck.calls).toHaveLength(1 + RESUME_TRIES);
  });
});

describe('the board calls of the views', () => {
  test('the activity path carries only the filters that are set', () => {
    expect(activityPath({})).toBe('/api/activity');
    expect(activityPath({ since: 12.7, limit: 50, session: 's 1', repo: '' })).toBe(
      '/api/activity?since=12&session=s+1&limit=50',
    );
    expect(activityPath({ repo: 'github.com/acme/web' })).toBe(
      '/api/activity?repo=github.com%2Facme%2Fweb',
    );
  });

  test('the read mark posts the seq to the session path with encoded ids', async () => {
    const calls: [string, unknown][] = [];
    const views = viewsActions(
      async () => ({}),
      async (path, body) => {
        calls.push([path, body]);
        return {};
      },
    );
    await views.markRead({ station_id: 'st/1', session_id: 's 2' }, 7);
    expect(calls).toEqual([['/api/sessions/st%2F1/s%202/read', { seq: 7 }]]);
  });

  const row = {
    station_id: 'st',
    session_id: 's1',
    session_title: null,
    repo: null,
    kind: 'message',
    text: 'done',
    at: 5,
    station_seq: 3,
  };

  test('the activity list validates every row', async () => {
    const views = viewsActions(
      async () => ({ items: [row] }),
      async () => ({}),
    );
    expect((await views.activity({})).items).toEqual([row as never]);
    expect(activityItem({ ...row, station_seq: null }).station_seq).toBeNull();
    expect(() => activityItem({ ...row, kind: 'nope' })).toThrow('invalid activity row');
    expect(() => activityItem({ ...row, at: 'x' })).toThrow('invalid activity row');
    const bad = viewsActions(
      async () => ({ nope: 1 }),
      async () => ({}),
    );
    expect(bad.activity({})).rejects.toThrow('invalid activity list');
  });
});
