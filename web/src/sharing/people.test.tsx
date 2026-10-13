import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { ChatView } from '../chat/ChatView';
import type { ChatMessage, DeckSession, Event, EventEnvelope } from '../gen';
import { sessionActions } from '../session/commands';
import { SessionView } from '../session/SessionView';
import { buildTimeline, type Item } from '../session/timeline';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import { claudeCapabilities, envelopes } from '../testing/timeline';
import { Avatars, presentNames } from './Avatars';
import { PeopleProvider } from './context';
import { env, fakePeopleCalls, member, memberEnv } from './fixtures';
import { startHeartbeat } from './hooks';
import { MemberBanner } from './MemberBanner';
import { actorLabel, memberName, ROLE_TEXT, roleAccess } from './member';
import { MobStrip } from './MobStrip';
import {
  actorName,
  clock,
  driveView,
  initial,
  mergeChat,
  MAX_AVATARS,
  peopleCalls,
} from './people';
import { SideChat } from './SideChat';

const msg = (seq: number, author: string, text = `m${seq}`): ChatMessage => ({
  seq,
  author,
  text,
  at: Date.UTC(2026, 9, 13, 9, seq),
});

describe('presence avatars (R11.5)', () => {
  test('round initials with the name as the tooltip', () => {
    const html = renderToStaticMarkup(<Avatars who={['owner', 'member:Ana']} self="member:Bo" />);
    expect(html).toContain('aria-label="People here"');
    expect(html).toContain('title="Owner"');
    expect(html).toContain('title="Ana"');
    expect(html).toContain('>O<');
    expect(html).toContain('>A<');
  });

  test('the owner is "You" for the owner and "Owner" for a member', () => {
    expect(presentNames(['owner', 'member:Ana'], 'owner')).toEqual(['You', 'Ana']);
    expect(presentNames(['owner', 'member:Ana'], 'member:Ana')).toEqual(['Owner', 'Ana']);
    expect(actorLabel('owner', 'owner')).toBe('You');
    expect(actorLabel('member:Ana', 'owner')).toBe('Ana');
  });

  test('five at most, and "+n" for the rest', () => {
    const who = ['owner', ...['Ana', 'Bo', 'Cy', 'Di', 'Ed', 'Fy'].map((n) => `member:${n}`)];
    const html = renderToStaticMarkup(<Avatars who={who} self="owner" />);
    expect(MAX_AVATARS).toBe(5);
    expect(html.match(/class="fd-avatar"/g)).toHaveLength(5);
    expect(html).toContain('>+2<');
    expect(html).not.toContain('title="Ed"');
  });

  test('nobody: nothing renders; a person is shown once', () => {
    expect(renderToStaticMarkup(<Avatars who={[]} self="owner" />)).toBe('');
    expect(presentNames(['member:Ana', 'member:Ana'], 'owner')).toEqual(['Ana']);
    expect(initial('  ana')).toBe('A');
    expect(initial('')).toBe('?');
  });

  test('the heartbeat sends now and then every 20 seconds, and stops', () => {
    const sent: string[] = [];
    const timers = new Map<number, { fn: () => void; ms: number }>();
    let next = 0;
    const stop = startHeartbeat(
      (k) => sent.push(k.session_id),
      { station_id: 's', session_id: 'one' },
      undefined,
      {
        set: (fn, ms) => {
          timers.set(++next, { fn, ms });
          return next;
        },
        clear: (id) => void timers.delete(id as number),
      },
    );
    expect(sent).toEqual(['one']);
    const timer = [...timers.values()][0];
    expect(timer?.ms).toBe(20_000);
    timer?.fn();
    expect(sent).toEqual(['one', 'one']);
    stop();
    expect(timers.size).toBe(0);
  });
});

describe('author labels (R11.5)', () => {
  const run = (author?: string): EventEnvelope[] =>
    envelopes('s1', [
      {
        kind: 'run.started',
        run_id: 'r1',
        input_summary: 'fix the build',
        origin: 'board',
        ...(author === undefined ? {} : { author }),
      } as Event,
    ]);
  const items = (events: EventEnvelope[]): readonly Item[] => buildTimeline(events).items;
  const chat = (events: EventEnvelope[]) =>
    renderToStaticMarkup(
      <ChatView items={items(events)} working={false} ready={true} error={null} />,
    );

  test('a prompt of a member shows its name after the time', () => {
    const html = chat(run('member:Ana'));
    expect(html).toMatch(/fd-chat-author">\d\d:\d\d — Ana</);
  });

  test('the owner shows nothing extra', () => {
    expect(chat(run())).not.toContain('fd-chat-author');
    expect(chat(run('token:ci'))).not.toContain('fd-chat-author');
  });

  test('a resolved approval shows who answered', () => {
    const events = envelopes('s1', [
      {
        kind: 'approval.requested',
        request_id: 'q1',
        node_id: null,
        request_kind: 'command',
        title: 'Run ls',
        details: 'ls',
        input: null,
        options: [{ option_id: 'allow', label: 'Allow' }],
        response: 'rpc_reply',
        provider_refs: {},
      },
      {
        kind: 'approval.resolved',
        request_id: 'q1',
        option_id: 'allow',
        by: 'board',
        answered_by: 'member:Bo',
      },
    ] as Event[]);
    expect(chat(events)).toContain('answered by Bo');
    const owner = envelopes('s1', [
      ...events.slice(0, 1).map((e) => e.event),
      { kind: 'approval.resolved', request_id: 'q1', option_id: 'allow', by: 'board' },
    ] as Event[]);
    expect(chat(owner)).not.toContain('answered by');
  });
});

describe('the side chat (R11.6)', () => {
  const view = (extra: Partial<Parameters<typeof SideChat>[0]> = {}) =>
    renderToStaticMarkup(
      <SideChat messages={[]} self="owner" onSend={async () => null} {...extra} />,
    );

  test('the note, the empty state and the input', () => {
    const html = view();
    expect(html).toContain('This chat is between people. The agent never sees it.');
    expect(html).toContain('No messages yet.');
    expect(html).toContain('maxLength="2000"');
    expect(html).toContain('Message to the people');
    expect(html).toMatch(/<button type="submit" class="fd-primary" disabled="">Send/);
  });

  test('messages: author, time and plain text', () => {
    const html = view({
      messages: [msg(1, 'member:Ana', '**bold** <b>x</b>'), msg(2, 'owner', 'hi')],
    });
    expect(html).toContain('<strong>Ana</strong>');
    expect(html).toContain('<strong>You</strong>');
    expect(html).toMatch(/<span>\d\d:\d\d<\/span>/);
    // Plain text: no Markdown and no HTML.
    expect(html).toContain('**bold** &lt;b&gt;x&lt;/b&gt;');
    expect(html).not.toContain('<b>');
    expect(html).not.toContain('fd-md');
  });

  test('loading and error states', () => {
    expect(view({ loading: true })).toContain('Loading the chat…');
    expect(view({ error: 'nope' })).toContain('role="alert"');
  });

  test('merging keeps one message for each seq, in order', () => {
    const a = [msg(1, 'owner'), msg(3, 'owner')];
    const merged = mergeChat(a, [msg(2, 'member:Ana'), msg(3, 'owner', 'again')]);
    expect(merged.map((m) => m.seq)).toEqual([1, 2, 3]);
    expect(merged[2]?.text).toBe('again');
    expect(clock(Date.UTC(2026, 9, 13, 9, 5))).toMatch(/^\d\d:\d\d$/);
  });

  test('the calls: list after a cursor, post, and the drive routes', async () => {
    const seen: string[] = [];
    const calls = peopleCalls(
      async (path) => {
        seen.push(`GET ${path}`);
        return [msg(1, 'owner')];
      },
      async (path, body) => {
        seen.push(`POST ${path} ${JSON.stringify(body)}`);
        return path.endsWith('/chat')
          ? msg(2, 'owner', 'x')
          : { driver: 'owner', since: 1, requests: [] };
      },
    );
    const key = { station_id: 'st a', session_id: 's1' };
    expect((await calls.chat(key, 7)).length).toBe(1);
    expect((await calls.say(key, 'hello')).seq).toBe(2);
    expect(await calls.requestDrive(key)).toEqual({ ok: true, text: 'Requested.' });
    expect(await calls.handOver(key, 'member:Bo')).toEqual({ ok: true, text: 'Handed over.' });
    expect(await calls.releaseDrive(key)).toEqual({ ok: true, text: 'Released.' });
    expect(seen).toEqual([
      'GET /api/sessions/st%20a/s1/chat?after=7',
      'POST /api/sessions/st%20a/s1/chat {"text":"hello"}',
      'POST /api/sessions/st%20a/s1/drive/request {}',
      'POST /api/sessions/st%20a/s1/drive/grant {"to":"member:Bo"}',
      'POST /api/sessions/st%20a/s1/drive/release {}',
    ]);
  });

  test('a refused drive call is an error text', async () => {
    const calls = peopleCalls(
      async () => [],
      async () => {
        throw new Error('only the driver or the owner hands on the composer');
      },
    );
    expect(await calls.handOver({ station_id: 's', session_id: 'x' }, 'owner')).toEqual({
      ok: false,
      text: 'only the driver or the owner hands on the composer',
    });
  });
});

describe('mob strip (R11.7)', () => {
  const base = {
    self: 'member:Ana',
    isMember: true,
    collaborator: true,
    shareMob: true,
    known: false,
    driver: null,
    requests: [] as string[],
  };

  test('a session that is not in mob mode has no strip', () => {
    expect(driveView({ ...base, shareMob: false })).toBeNull();
    expect(driveView({ ...base, shareMob: false, known: true })).not.toBeNull();
  });

  test('nobody drives: a member can request', () => {
    const v = driveView(base);
    expect(v?.text).toBe('Nobody is driving');
    expect(v).toMatchObject({
      locked: true,
      canRequest: true,
      canRelease: false,
      canHandOver: false,
    });
    const html = renderToStaticMarkup(<MobStrip view={v ?? ({} as never)} />);
    expect(html).toContain('Nobody is driving');
    expect(html).toContain('Request to drive');
    expect(html).not.toContain('Release');
  });

  test('another member drives: "<name> is driving", the composer stays locked', () => {
    const v = driveView({ ...base, driver: 'member:Bo' });
    expect(v?.text).toBe('Bo is driving');
    expect(v?.locked).toBe(true);
    expect(v?.canRequest).toBe(true);
    expect(driveView({ ...base, driver: 'owner' })?.text).toBe('Owner is driving');
  });

  test('a member that asked waits; it cannot ask twice', () => {
    const v = driveView({ ...base, driver: 'member:Bo', requests: ['member:Ana'] });
    expect(v).toMatchObject({ requested: true, canRequest: false });
    const html = renderToStaticMarkup(<MobStrip view={v ?? ({} as never)} />);
    expect(html).toContain('Drive requested');
    expect(html).not.toContain('Request to drive');
  });

  test('the driver sees the requests with "Hand over" and "Release"', () => {
    const v = driveView({
      ...base,
      driver: 'member:Ana',
      requests: ['member:Bo', 'member:Cy'],
    });
    expect(v).toMatchObject({ driving: true, locked: false, canHandOver: true, canRelease: true });
    expect(v?.text).toBe('You are driving');
    const html = renderToStaticMarkup(<MobStrip view={v ?? ({} as never)} />);
    expect(html).toContain('Bo asks to drive');
    expect(html).toContain('aria-label="Hand over to Cy"');
    expect(html.match(/>Hand over</g)).toHaveLength(2);
    expect(html).toContain('>Release<');
  });

  test('the owner always has the composer and sees the requests, but has no Release', () => {
    const v = driveView({
      ...base,
      self: 'owner',
      isMember: false,
      shareMob: false,
      known: true,
      driver: 'member:Bo',
      requests: ['member:Cy'],
    });
    expect(v).toMatchObject({
      locked: false,
      canHandOver: true,
      canRelease: false,
      canRequest: false,
    });
    expect(v?.text).toBe('Bo is driving');
    const html = renderToStaticMarkup(<MobStrip view={v ?? ({} as never)} />);
    expect(html).toContain('Hand over');
    expect(html).not.toContain('>Release<');
  });

  test('a viewer does not request and a viewer or an approver is locked', () => {
    expect(driveView({ ...base, collaborator: false })?.canRequest).toBe(false);
  });

  test('a refusal is shown as an alert', () => {
    const v = driveView(base);
    const html = renderToStaticMarkup(
      <MobStrip view={v ?? ({} as never)} note={{ ok: false, text: 'Ask to drive first.' }} />,
    );
    expect(html).toContain('role="alert"');
    expect(html).toContain('Ask to drive first.');
    expect(actorName('member:Bo')).toBe('Bo');
  });

  test('a 403 not_driver on a send is "Ask to drive first."', async () => {
    const { ApiError } = await import('../runtime/api');
    const actions = sessionActions(
      async () => {
        throw new ApiError(403, 'this session is in mob mode: ask to drive first', 'not_driver');
      },
      { station_id: 'station-a', session_id: 's1' },
      ['read', 'drive'],
    );
    expect(await actions.send('prompt', 'hi')).toEqual({ ok: false, text: 'Ask to drive first.' });
    const other = sessionActions(
      async () => {
        throw new ApiError(403, 'forbidden', 'forbidden');
      },
      { station_id: 'station-a', session_id: 's1' },
      ['read', 'drive'],
    );
    expect(await other.send('prompt', 'hi')).toEqual({ ok: false, text: 'forbidden' });
  });
});

describe('member names and roles', () => {
  test('the name of an actor', () => {
    expect(memberName('member:Ana')).toBe('Ana');
    expect(memberName('member:')).toBeNull();
    expect(memberName('owner')).toBeNull();
  });

  test('what each role shows', () => {
    expect(roleAccess('viewer')).toEqual({ composer: false, approvals: false });
    expect(roleAccess('approver')).toEqual({ composer: false, approvals: true });
    expect(roleAccess('collaborator')).toEqual({ composer: true, approvals: true });
    expect(Object.keys(ROLE_TEXT)).toEqual(['viewer', 'collaborator', 'approver']);
  });

  test('the banner: role, owner, name and Leave', () => {
    const html = renderToStaticMarkup(
      <MemberBanner member={member('collaborator', 'Ana')} onLeave={() => {}} />,
    );
    expect(html).toContain('Shared with you as collaborator by the owner · Ana');
    expect(html).toContain('>Leave<');
    expect(
      renderToStaticMarkup(<MemberBanner member={member('viewer')} onLeave={undefined} />),
    ).not.toContain('Leave');
  });
});

const key = { station_id: 'station-a', session_id: 's1' };
const card = (extra: Partial<DeckSession['summary']> = {}): DeckSession => ({
  station_id: 'station-a',
  status: 'needs_you',
  summary: summary('s1', 'needs_you', 1, {
    title: 'Review',
    capabilities: claudeCapabilities(),
    mode: 'default',
    open_requests: [
      {
        request_id: 'q1',
        node_id: null,
        request_kind: 'command',
        title: 'Run ls',
        details: 'ls',
        input: null,
        options: [{ option_id: 'allow', label: 'Allow' }],
        response: 'rpc_reply',
        requested_at: 1,
      },
    ],
    ...extra,
  }),
});

const pane = (
  peopleEnv: ReturnType<typeof env>,
  scopes: string[],
  tab: 'chat' | 'people' = 'chat',
) =>
  renderToStaticMarkup(
    <PeopleProvider value={peopleEnv}>
      <SessionView
        session={card()}
        stations={[station()]}
        freshness="live"
        timeline={{
          incarnation: 'i',
          cursor: 0,
          events: [],
          freshness: 'live',
          error: null,
          ready: true,
        }}
        actions={sessionActions(
          async () => ({}),
          key,
          scopes,
          undefined,
          async () => ({}),
        )}
        layout="phone"
        tab={tab}
      />
    </PeopleProvider>,
  );

const SCOPES = {
  owner: ['read', 'drive', 'approve', 'admin'],
  viewer: ['read'],
  collaborator: ['read', 'drive'],
  approver: ['read', 'drive', 'approve'],
};

describe('member mode on a session (R11.4)', () => {
  test('the owner: composer, approvals, the full menu and Share session…', () => {
    const html = pane(env({ shares: { shares: async () => [] } as never }), SCOPES.owner);
    expect(html).toContain('aria-label="Composer"');
    expect(html).toContain('aria-label="Needs you"');
    expect(html).toContain('Rename');
    expect(html).toContain('Stop');
    expect(html).toContain('Share session…');
    expect(html).not.toContain('Shared with you');
  });

  test('a viewer: "You are a viewer.", no composer, no approval buttons, no owner menu', () => {
    const html = pane(memberEnv('viewer'), SCOPES.viewer);
    expect(html).toContain('You are a viewer.');
    expect(html).not.toContain('aria-label="Composer"');
    expect(html).not.toContain('aria-label="Needs you"');
    expect(html).not.toContain('>Allow<');
    for (const text of [
      'Rename',
      'Stop',
      'Share session…',
      'Open shell',
      'Force stop',
      'Ask for a status update',
    ]) {
      expect(html).not.toContain(text);
    }
    expect(html).not.toContain('aria-label="Model"');
    expect(html).not.toContain('aria-label="Mode"');
  });

  test('an approver: approvals, no composer, no viewer note', () => {
    const html = pane(memberEnv('approver'), SCOPES.approver);
    expect(html).toContain('aria-label="Needs you"');
    expect(html).toContain('>Allow<');
    expect(html).not.toMatch(/<button[^>]*disabled=""[^>]*>Allow/);
    expect(html).not.toContain('aria-label="Composer"');
    expect(html).not.toContain('You are a viewer.');
    expect(html).not.toContain('Rename');
    expect(html).not.toContain('Ask for a status update');
  });

  test('a collaborator: composer and approvals, the status update, no owner menu', () => {
    const html = pane(memberEnv('collaborator'), SCOPES.collaborator);
    expect(html).toContain('aria-label="Composer"');
    expect(html).toContain('aria-label="Needs you"');
    expect(html).toContain('>Allow<');
    expect(html).toContain('Ask for a status update');
    for (const text of ['Rename', 'Force stop', 'Share session…', 'Open shell', 'Resume']) {
      expect(html).not.toContain(text);
    }
  });

  test('a collaborator of a mob share who does not drive: the composer is off', () => {
    const peopleEnv = memberEnv('collaborator', {
      calls: fakePeopleCalls(),
      driver: { [JSON.stringify(['station-a', 's1'])]: 'owner' },
      driveRequests: { [JSON.stringify(['station-a', 's1'])]: [] },
    });
    const html = pane(peopleEnv, SCOPES.collaborator);
    expect(html).toContain('aria-label="Mob steering"');
    expect(html).toContain('Owner is driving');
    expect(html).toContain('Request to drive');
    expect(html).toMatch(/<textarea[^>]*disabled=""/);
    expect(html).toContain('Request to drive to send prompts.');
  });

  test('the driver of a mob session has the composer, Release and the requests', () => {
    const k = JSON.stringify(['station-a', 's1']);
    const html = pane(
      memberEnv('collaborator', {
        calls: fakePeopleCalls(),
        driver: { [k]: 'member:Ana' },
        driveRequests: { [k]: ['member:Bo'] },
      }),
      SCOPES.collaborator,
    );
    expect(html).toContain('You are driving');
    expect(html).toContain('>Release<');
    expect(html).toContain('Hand over');
    expect(html).not.toMatch(/<textarea[^>]*disabled=""/);
  });

  test('the owner sees the strip of a mob session and keeps the composer', () => {
    const k = JSON.stringify(['station-a', 's1']);
    const html = pane(
      env({
        calls: fakePeopleCalls(),
        driver: { [k]: 'member:Bo' },
        driveRequests: { [k]: ['member:Cy'] },
      }),
      SCOPES.owner,
    );
    expect(html).toContain('Bo is driving');
    expect(html).toContain('Cy asks to drive');
    expect(html).not.toMatch(/<textarea[^>]*disabled=""/);
  });

  test('the avatars show in the header', () => {
    const k = JSON.stringify(['station-a', 's1']);
    const html = pane(env({ presence: { [k]: ['owner', 'member:Ana'] } }), SCOPES.owner);
    expect(html).toContain('aria-label="People here"');
    expect(html).toContain('title="You"');
    expect(html).toContain('title="Ana"');
  });
});

describe('the People tab (R11.6)', () => {
  const k = JSON.stringify(['station-a', 's1']);
  test('it is hidden for one person without chat', () => {
    const html = pane(env({ presence: { [k]: ['owner'] } }), SCOPES.owner);
    expect(html).not.toContain('>People<');
  });

  test('two people show it, with the count', () => {
    const html = pane(env({ presence: { [k]: ['owner', 'member:Ana'] } }), SCOPES.owner);
    expect(html).toMatch(/role="tab"[^>]*>People <span class="fd-count">2<\/span>/);
  });

  test('a chat message shows it', () => {
    const html = pane(env({ chat: { [k]: [msg(1, 'member:Ana')] } }), SCOPES.owner);
    expect(html).toContain('>People<');
  });

  test('the owner of a shared session always has it', () => {
    const html = pane(env({ shared: new Set([k]) }), SCOPES.owner);
    expect(html).toContain('>People<');
    expect(pane(env({ shared: new Set(['*']) }), SCOPES.owner)).toContain('>People<');
  });

  test('the tab shows the chat of the session', () => {
    const html = pane(
      env({ chat: { [k]: [msg(1, 'member:Ana', 'hello team')] } }),
      SCOPES.owner,
      'people',
    );
    expect(html).toContain('This chat is between people. The agent never sees it.');
    expect(html).toContain('hello team');
    expect(html).toContain('aria-selected="true"');
  });

  test('a tab that is not offered shows the chat', () => {
    const html = pane(env(), SCOPES.owner, 'people');
    expect(html).not.toContain('This chat is between people');
    expect(html).toContain('aria-label="Composer"');
  });
});
