import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { card, station } from '../testing/fleet';
import { canShare, PeopleProvider } from './context';
import { fakeShareCalls, NOW, DAY, env, share, space } from './fixtures';
import { sharedSets } from './ownerShares';
import {
  createShare,
  defaultForm,
  EXPIRY_DAYS,
  NewShareDialog,
  ShareDialog,
  ShareFields,
  ShareResult,
  shareBody,
  shareTargets,
  USES,
} from './ShareDialog';
import {
  SharesPage,
  SharesView,
  shareHistoryOff,
  shareHistoryOn,
  shareState,
  targetText,
} from './SharesPage';
import { closeShare, openShare, sharesChanged } from './shareStore';
import { SpaceCard, SpacesPage, SpacesView } from './SpacesPage';

const request = { kind: 'session', target: 'station-a/s1', label: 'Review' } as const;
const dialog = (extra: Partial<Parameters<typeof ShareDialog>[0]> = {}) =>
  renderToStaticMarkup(
    <ShareDialog request={request} calls={fakeShareCalls()} onClose={() => {}} {...extra} />,
  );

describe('share dialog (R11.4)', () => {
  test('the fields: three roles with their lines, expiry and uses', () => {
    const html = dialog();
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-label="Share session"');
    expect(html).toContain('sees the chat, the files and the changes');
    expect(html).toContain('also sends prompts and steers');
    expect(html).toContain('also answers approvals and questions');
    expect(html.match(/type="radio"/g)).toHaveLength(3);
    expect(html).toMatch(/checked="" value="viewer"/);
    for (const label of ['1 day', '7 days', '30 days', '90 days']) expect(html).toContain(label);
    for (const label of ['1 person', '5 people', '25 people']) expect(html).toContain(label);
    expect(html).toMatch(/<option value="7" selected/);
    expect(html).toMatch(/<option value="1" selected/);
    expect(html).not.toContain('Steering');
  });

  test('the defaults and the choices', () => {
    expect(EXPIRY_DAYS).toEqual([1, 7, 30, 90]);
    expect(USES).toEqual([1, 5, 25]);
    expect(defaultForm).toEqual({
      role: 'viewer',
      steering: 'open',
      days: 7,
      uses: 1,
      history: false,
    });
  });

  test('steering shows for a collaborator only', () => {
    const html = renderToStaticMarkup(
      <ShareFields
        request={request}
        form={{ ...defaultForm, role: 'collaborator' }}
        busy={false}
        onChange={() => {}}
        onSubmit={() => {}}
      />,
    );
    expect(html).toContain('Steering');
    expect(html).toContain('Open (anyone steers)');
    expect(html).toContain('Mob (one driver at a time)');
    expect(html).toMatch(/checked="" value="open"/);
  });

  test('the body of the call: steering only for the roles that steer', () => {
    expect(shareBody(request, defaultForm)).toEqual({
      kind: 'session',
      target: 'station-a/s1',
      role: 'viewer',
      expires_in_days: 7,
      uses: 1,
    });
    expect(
      shareBody(
        { kind: 'board', target: '', label: '' },
        { role: 'collaborator', steering: 'mob', days: 30, uses: 5, history: false },
      ),
    ).toEqual({
      kind: 'board',
      target: '',
      role: 'collaborator',
      steering: 'mob',
      expires_in_days: 30,
      uses: 5,
    });
    expect(
      shareBody(request, { role: 'approver', steering: 'mob', days: 1, uses: 25, history: false })
        .steering,
    ).toBe('mob');
    expect(
      shareBody(request, { role: 'viewer', steering: 'mob', days: 1, uses: 25, history: false })
        .steering,
    ).toBe(undefined);
  });

  test('the result: the URL with Copy, the QR code and who can join', () => {
    const html = dialog({
      initialLink: {
        link: { share_id: 'sh1', url: 'https://deck.test/#join=abc', expires_at: NOW + 7 * DAY },
        role: 'collaborator',
      },
    });
    expect(html).toContain('value="https://deck.test/#join=abc"');
    expect(html).toContain('Copy link');
    expect(html).toContain('aria-label="QR code of the invite link"');
    expect(html).toContain('Anyone with this link can join as collaborator until 2026-10-20.');
    expect(html).toContain('Revoke it in Shares.');
  });

  test('the title follows the kind, and an error is an alert', () => {
    expect(dialog({ request: { kind: 'board', target: '', label: '' } })).toContain(
      'aria-label="Share board"',
    );
    expect(dialog({ request: { kind: 'space', target: 'sp1', label: 'Alpha' } })).toContain(
      'aria-label="Share space"',
    );
    const html = dialog({ initialError: 'owner only' });
    expect(html).toContain('role="alert"');
    expect(html).toContain('owner only');
  });

  test('the result alone', () => {
    const html = renderToStaticMarkup(
      <ShareResult
        link={{ share_id: 's', url: 'https://d/#join=x', expires_at: NOW }}
        joinAs="viewer"
      />,
    );
    expect(html).toContain('join as viewer until 2026-10-13.');
  });

  test('openShare asks for the dialog and closeShare ends it', () => {
    openShare(request);
    closeShare();
    sharesChanged();
  });

  test('only the owner login with share calls can share', () => {
    expect(canShare(env())).toBe(false);
    expect(canShare(env({ shares: fakeShareCalls() }))).toBe(true);
    expect(
      canShare(
        env({
          shares: fakeShareCalls(),
          member: { role: 'viewer', name: 'Ana', scope: null },
        }),
      ),
    ).toBe(false);
  });
});

const sessions = [card('s1', 'working', 'station-a', { title: 'Review' }), card('s2')];
const stations = [station('station-a')];

describe('shares page (R11.4)', () => {
  const view = (state: Parameters<typeof SharesView>[0]['state'], extra = {}) =>
    renderToStaticMarkup(
      <SharesView state={state} now={NOW} sessions={sessions} stations={stations} {...extra} />,
    );

  test('loading, error and empty states', () => {
    expect(view({ kind: 'loading' })).toContain('Loading the shares…');
    const error = view({ kind: 'error', text: 'owner only' });
    expect(error).toContain('role="alert"');
    expect(error).toContain('Try again');
    const empty = view({ kind: 'ok', value: [] });
    expect(empty).toContain('No shares yet');
    expect(empty).toContain(
      'Share a session, a space or the whole board with a link. People join with a name you can see, in the role you pick.',
    );
    // One primary per screen: while the list is empty, only the empty state offers "New share".
    expect(empty.match(/New share<\/button>/g)).toHaveLength(1);
    expect(empty).toContain('class="fd-primary"');
  });

  test('a row: kind, target, role, steering, members, uses, expiry', () => {
    const html = view({
      kind: 'ok',
      value: [
        share('a', {
          role: 'collaborator',
          steering: 'mob',
          members: ['Ana', 'Bo'],
          uses_left: 3,
        }),
      ],
    });
    expect(html).toContain('Session</span>');
    expect(html).toContain('Alpha / Review');
    expect(html).toContain('>Collaborator<');
    expect(html).toContain('>Mob<');
    expect(html).toContain('<table class="fd-table" aria-label="Shares">');
    for (const head of ['Role', 'Members', 'Uses left', 'Expires']) expect(html).toContain(head);
    expect(html).toContain('data-label="Members">Ana, Bo</td>');
    expect(html).toContain('data-label="Uses left" class="fd-num">3</td>');
    expect(html).toContain('in 5 d<');
    expect(html).toContain('title="2026-10-18"');
    expect(html).toContain('aria-label="Revoke the share of Alpha / Review"');
  });

  test('revoked and expired shares have a badge and no Revoke', () => {
    const html = view({
      kind: 'ok',
      value: [
        share('r', { revoked_at: NOW - 1000, uses_left: 0 }),
        share('e', { expires_at: NOW - 1000 }),
      ],
    });
    expect(html).toContain('Revoked');
    expect(html).toContain('Expired');
    expect(html).toContain('revoked 2026-10-13');
    expect(html).not.toContain('>Revoke<');
    expect(shareState(share('r', { revoked_at: 1 }), NOW)).toBe('revoked');
    expect(shareState(share('e', { expires_at: NOW }), NOW)).toBe('expired');
    expect(shareState(share('l'), NOW)).toBe('live');
  });

  test('the revoke asks for a confirm first', () => {
    const html = view({ kind: 'ok', value: [share('a')] }, { confirming: 'a' });
    expect(html).toContain('role="group" aria-label="Revoke the share"');
    expect(html).toContain('Revoke this share? The members lose access at once.');
    expect(html).toContain('Cancel');
    expect(view({ kind: 'ok', value: [share('a')] }, { confirming: 'a', revoking: 'a' })).toContain(
      'Revoking…',
    );
  });

  test('the target of a board share and of a space share', () => {
    expect(targetText(share('b', { kind: 'board', target: '' }), sessions, stations)).toBe(
      'The whole board',
    );
    expect(targetText(share('c', { kind: 'space', target: 'sp9' }), sessions, stations)).toBe(
      'Space sp9',
    );
    expect(targetText(share('d', { target: 'station-a/gone' }), sessions, stations)).toBe(
      'Alpha / gone',
    );
  });

  test('the page of a member or a token says that only the owner sees it', () => {
    const html = renderToStaticMarkup(
      <PeopleProvider value={env()}>
        <SharesPageProbe />
      </PeopleProvider>,
    );
    expect(html).toContain('Only the owner of the deck sees the shares.');
  });
});

const SharesPageProbe = () => <SharesPage sessions={sessions} stations={stations} />;

describe('spaces page (R11.3)', () => {
  const view = (state: Parameters<typeof SpacesView>[0]['state']) =>
    renderToStaticMarkup(<SpacesView state={state} sessions={sessions} stations={stations} />);

  test('loading, error and empty states', () => {
    expect(view({ kind: 'loading' })).toContain('Loading the spaces…');
    expect(view({ kind: 'error', text: 'nope' })).toContain('role="alert"');
    const empty = view({ kind: 'ok', value: [] });
    expect(empty).toContain('No spaces yet');
    // The empty state holds New space (one primary); a fresh page shows the button, not the form.
    expect(empty).toContain('New space</button>');
    expect(empty).not.toContain('aria-label="New space"');
    const open = renderToStaticMarkup(
      <SpacesView
        state={{ kind: 'ok', value: [] }}
        sessions={sessions}
        stations={stations}
        initialCreating
      />,
    );
    expect(open).toContain('aria-label="New space"');
    expect(open).toContain('Create space');
    expect(open).toContain('maxLength="80"');
    expect(open).not.toContain('class="fd-primary"><svg');
  });

  test('a space lists its sessions, "Add a session" and "Share space…"', () => {
    const html = view({
      kind: 'ok',
      value: [space('sp1', 'Alpha', [{ station_id: 'station-a', session_id: 's1' }])],
    });
    expect(html).toContain('>Alpha<');
    expect(html).toContain('Alpha / Review');
    // Only the sessions that are not in the space can be added.
    expect(html).toContain('Alpha / s2');
    expect(html.match(/<option value="station-a\/s1"/g)).toBeNull();
    expect(html).toContain('Add a session');
    expect(html).toContain('Share space…');
  });

  test('a space without sessions, and nothing left to add', () => {
    const html = renderToStaticMarkup(
      <table>
        <tbody>
          <SpaceCard
            space={space('sp2', 'Empty')}
            sessions={[]}
            stations={stations}
            busy={false}
            onAdd={() => {}}
            onShare={() => {}}
          />
        </tbody>
      </table>,
    );
    expect(html).toContain('No sessions in this space yet.');
    expect(html).toContain('No more sessions');
    expect(html).toMatch(/<select[^>]*disabled/);
  });

  test('the page of a non-owner', () => {
    const html = renderToStaticMarkup(
      <PeopleProvider value={env()}>
        <SpacesPage sessions={sessions} stations={stations} />
      </PeopleProvider>,
    );
    expect(html).toContain('Only the owner of the deck sees the spaces.');
  });
});

describe('the sessions that the owner shares', () => {
  const s1 = JSON.stringify(['station-a', 's1']);
  const s2 = JSON.stringify(['station-a', 's2']);
  test('a live share covers its session, its space and the board', () => {
    const sets = sharedSets(
      [
        share('a', { steering: 'mob' }),
        share('b', { kind: 'space', target: 'sp1' }),
        share('c', { revoked_at: 1, target: 'station-a/s3' }),
        share('d', { expires_at: NOW - 1, target: 'station-a/s4' }),
      ],
      [space('sp1', 'Alpha', [{ station_id: 'station-a', session_id: 's2' }])],
      NOW,
    );
    expect([...sets.shared].sort()).toEqual([s1, s2].sort());
    expect([...sets.mob]).toEqual([s1]);
  });

  test('a board share covers every session', () => {
    const sets = sharedSets([share('a', { kind: 'board', target: '', steering: 'mob' })], [], NOW);
    expect(sets.shared.has('*')).toBe(true);
    expect(sets.mob.has('*')).toBe(true);
  });
});

describe('new share (DL-171)', () => {
  test('the targets: the board, the spaces, then the sessions by title', () => {
    const targets = shareTargets(sessions, stations, [space('sp1', 'Alpha team')]);
    expect(targets.slice(0, 3).map((t) => t.label)).toEqual([
      'The whole board',
      'Space Alpha team',
      'Review (Alpha)',
    ]);
    expect(targets).toHaveLength(4);
    expect(targets[0]?.request).toEqual({ kind: 'board', target: '', label: '' });
    expect(targets[1]?.request).toEqual({ kind: 'space', target: 'sp1', label: 'Alpha team' });
    expect(targets[2]?.request).toEqual({
      kind: 'session',
      target: 'station-a/s1',
      label: 'Review',
    });
  });

  test('without a space there is no Spaces group', () => {
    const targets = shareTargets(sessions, stations, []);
    expect(targets.some((t) => t.group === 'space')).toBe(false);
    expect(targets[0]?.group).toBe('board');
  });

  test('the dialog has a target select and the title New share', () => {
    const html = renderToStaticMarkup(
      <NewShareDialog
        sessions={sessions}
        stations={stations}
        spaces={[space('sp1', 'Alpha team')]}
        calls={fakeShareCalls()}
        onClose={() => {}}
      />,
    );
    expect(html).toContain('aria-label="New share"');
    expect(html).toContain('What to share');
    expect(html).toContain('>The whole board<');
    expect(html).toContain('<optgroup label="Spaces">');
    expect(html).toContain('>Space Alpha team<');
    expect(html).toContain('<optgroup label="Sessions">');
    expect(html).toContain('>Review (Alpha)<');
  });
});

describe('the history option of a share (DL-189)', () => {
  const warning =
    'Members see the full earlier conversation of the shared sessions, including anything that was pasted into it. Tool commands and file names stay hidden.';
  const history = { ...defaultForm, history: true };

  test('the body has history and elevation only when the option is on', () => {
    expect('history' in shareBody(request, defaultForm)).toBe(false);
    expect('elevation' in shareBody(request, defaultForm, 'tok')).toBe(false);
    expect(shareBody(request, history, 'tok')).toMatchObject({ history: true, elevation: 'tok' });
    expect('elevation' in shareBody(request, history)).toBe(false);
  });

  test('the checkbox and its text render, off by default', () => {
    const html = dialog();
    expect(html).toContain('type="checkbox"');
    expect(html).not.toMatch(/type="checkbox" checked/);
    expect(html).toContain('Members can read the earlier history of imported sessions');
    expect(html).toContain(warning);
    expect(dialog({ initialForm: history })).toMatch(/type="checkbox" checked/);
  });

  test('a create with history asks for the elevation and sends it', async () => {
    const bodies: unknown[] = [];
    const asked: (readonly string[] | undefined)[] = [];
    const link = { share_id: 'sh1', url: 'https://d/#join=x', expires_at: NOW };
    const done = await createShare(
      {
        create: async (body) => {
          bodies.push(body);
          return link;
        },
      },
      async (labels) => {
        asked.push(labels);
        return 'tok';
      },
      request,
      history,
    );
    expect(done).toEqual(link);
    expect(asked).toEqual([['Share the earlier history']]);
    expect(bodies).toHaveLength(1);
    expect(bodies[0]).toMatchObject({ history: true, elevation: 'tok' });
  });

  test('a create without history does not ask for the elevation', async () => {
    let asked = 0;
    const link = { share_id: 'sh1', url: 'https://d/#join=x', expires_at: NOW };
    const done = await createShare(
      { create: async () => link },
      async () => {
        asked += 1;
        return 'tok';
      },
      request,
      defaultForm,
    );
    expect(done).toEqual(link);
    expect(asked).toBe(0);
  });

  test('a cancelled elevation sends nothing', async () => {
    let sent = 0;
    const done = await createShare(
      {
        create: async () => {
          sent += 1;
          throw new Error('not used');
        },
      },
      async () => null,
      request,
      history,
    );
    expect(done).toBeNull();
    expect(sent).toBe(0);
  });

  const row = (extra: Partial<Parameters<typeof SharesView>[0]> = {}, history = false) =>
    renderToStaticMarkup(
      <SharesView
        state={{ kind: 'ok', value: [share('a', { history })] }}
        now={NOW}
        sessions={sessions}
        stations={stations}
        {...extra}
      />,
    );

  test('the badge shows when the history is shared', () => {
    expect(row({}, true)).toContain('History shared');
    expect(row({}, false)).not.toContain('History shared');
  });

  test('Share history opens the confirm with the warning', () => {
    const closed = row();
    expect(closed).toContain('aria-label="Share history of Alpha / Review"');
    expect(closed).not.toContain(warning);
    const open = row({ confirming: 'history:a' });
    expect(open).toContain(warning);
    expect(open).toContain('aria-label="Cancel sharing history of Alpha / Review"');
    // The revoke confirm and the history confirm are different groups.
    expect(open).not.toContain('Revoke this share?');
  });

  test('Stop sharing history shows when the option is on, with no confirm', () => {
    const html = row({}, true);
    expect(html).toContain('aria-label="Stop sharing history of Alpha / Review"');
    expect(html).not.toContain('aria-label="Share history of');
  });

  test('turning the history on asks for the elevation first', async () => {
    const calls: unknown[][] = [];
    const info = share('a', { history: true });
    const setHistory = async (...args: unknown[]) => {
      calls.push(args);
      return info;
    };
    expect(await shareHistoryOn({ setHistory }, async () => 'tok', 'a')).toEqual(info);
    expect(calls).toEqual([['a', true, 'tok']]);
    expect(await shareHistoryOn({ setHistory }, async () => null, 'a')).toBeNull();
    expect(calls).toHaveLength(1);
  });

  test('turning the history off sends enabled false and no elevation', async () => {
    const calls: unknown[][] = [];
    const info = share('a');
    const setHistory = async (...args: unknown[]) => {
      calls.push(args);
      return info;
    };
    expect(await shareHistoryOff({ setHistory }, 'a')).toEqual(info);
    expect(calls).toEqual([['a', false]]);
  });
});
