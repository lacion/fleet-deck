import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { AppOpenCall, DeckSession } from '../gen';
import { ApiError } from '../runtime/api';
import { summary } from '../testing/summary';
import type { AppActions } from './actions';
import { argsText, createDialogHost, dialogTexts } from './AppDialogs';
import {
  AppCalls,
  CallLines,
  callLine,
  OTHER_SESSION,
  ReleaseDialog,
  releaseCall,
  STILL_RUNNING,
} from './AppCalls';
import { networkNote, STOPPED_NOTE } from './AppFrame';
import {
  AppRow,
  AppsProvider,
  MEMBER_NOTE,
  StateBody,
  stateOfClose,
  stateOfViewError,
  type ToolApp,
} from './AppRow';
import {
  type AppsEnv,
  appsEnvOf,
  callsVersion,
  createFrameSlots,
  refreshCalls,
  registerAppActions,
  subscribeCalls,
} from './env';

const app: ToolApp = {
  app_id: '01JABCDEFGHJKMNPQRSTVWXYZ0',
  server: 'demo',
  tool: 'show_counter',
  prefers_border: undefined,
};

const never = (): never => {
  throw new Error('not called');
};
const actions = (over: Partial<AppActions> = {}): AppActions => ({
  view: never,
  call: never,
  calls: async () => ({ calls: [] }),
  release: async () => ({ released: true }),
  ...over,
});

const env = (over: Partial<AppsEnv> = {}): AppsEnv => ({
  station: 'st-1',
  session: 'ses-1',
  owner: true,
  actions: actions(),
  sendMessage: async () => {},
  revertKey: null,
  refreshCalls: () => {},
  ...over,
});

const call = (over: Partial<AppOpenCall> = {}): AppOpenCall => ({
  call_id: '01JCALL0000000000000000001',
  session_id: 'ses-1',
  app_id: app.app_id,
  server: 'demo',
  tool: 'bump',
  state: 'running',
  created_at: 1,
  ...over,
});

const html = (el: React.ReactElement) => renderToStaticMarkup(el);

describe('app row', () => {
  test('member sees the owner-only note and no frame', () => {
    const out = html(
      <AppsProvider env={env({ owner: false })}>
        <AppRow app={app} />
      </AppsProvider>,
    );
    expect(out).toContain('App from demo');
    expect(out).toContain(MEMBER_NOTE);
    expect(MEMBER_NOTE).toBe('Only the owner can open this app.');
    expect(out).not.toContain('<iframe');
  });

  test('no apps environment renders no row', () => {
    expect(
      html(
        <AppsProvider env={undefined}>
          <AppRow app={app} />
        </AppsProvider>,
      ),
    ).toBe('');
  });

  test('the owner row has a caption with the server and the tool', () => {
    const out = html(
      <AppsProvider env={env()}>
        <AppRow app={app} />
      </AppsProvider>,
    );
    expect(out).toContain('App from demo');
    expect(out).toContain('<code class="fd-app-tool">show_counter</code>');
    expect(out).toContain('Loading the app…');
  });

  test('at most three frames are mounted', () => {
    const slots = createFrameSlots(3);
    const evicted: string[] = [];
    for (const k of ['a', 'b', 'c', 'd']) slots.claim(k, () => evicted.push(k));
    expect(slots.count()).toBe(3);
    expect(evicted).toEqual(['a']);
    slots.release('b');
    slots.claim('e', () => evicted.push('e'));
    expect(slots.count()).toBe(3);
    expect(evicted).toEqual(['a']);
    slots.claim('f', () => evicted.push('f'));
    expect(evicted).toEqual(['a', 'c']);
  });

  test('network note lists the domains and is absent without them', () => {
    expect(networkNote([])).toBeNull();
    expect(networkNote(['https://a.example', 'https://b.example'])).toBe(
      'This app can connect to: https://a.example, https://b.example',
    );
    expect(STOPPED_NOTE).toBe('The session is not running. Resume it to use this app.');
  });
});

describe('dialogs', () => {
  test('consent dialog texts', () => {
    const t = dialogTexts({
      kind: 'tool',
      server: 'demo',
      tool: 'bump',
      title: 'Bump the counter',
      args: { by: 1 },
    });
    expect(t.title).toBe('Allow this app action?');
    expect(t.description).toBe(
      "The app from demo wants to run Bump the counter. It runs with the session's access to demo.",
    );
    expect(t.yes).toBe('Allow');
    expect(argsText({ by: 1 })).toBe('{\n  "by": 1\n}');
    const bare = dialogTexts({ kind: 'tool', server: 'demo', tool: 'bump', title: null, args: {} });
    expect(bare.description).toContain('wants to run bump.');
  });

  test('message and link dialog texts', () => {
    expect(dialogTexts({ kind: 'message', text: 'hi' }).title).toBe(
      'Send this message to the session?',
    );
    expect(dialogTexts({ kind: 'message', text: 'hi' }).yes).toBe('Send');
    expect(dialogTexts({ kind: 'link', url: 'https://x.example/' }).title).toBe('Open this link?');
    expect(dialogTexts({ kind: 'link', url: 'https://x.example/' }).yes).toBe('Open link');
  });

  test('Allow answers true, Cancel false, and a dropped dialog false', async () => {
    const shown: (string | null)[] = [];
    const host = createDialogHost((d) => shown.push(d?.kind ?? null));
    const allow = host.ask({ kind: 'tool', server: 's', tool: 't', title: null, args: {} });
    const second = host.ask({ kind: 'link', url: 'https://x.example/' });
    expect(shown).toEqual(['tool']);
    host.answer(true);
    expect(await allow).toBe(true);
    expect(shown).toEqual(['tool', 'link']);
    host.dropAll();
    expect(await second).toBe(false);
    expect(host.current()).toBeNull();
  });
});

describe('row states', () => {
  const body = (state: Parameters<typeof StateBody>[0]['state']) =>
    html(<StateBody state={state} onShow={() => {}} onReload={() => {}} />);

  test('error states (offline, gone, load error, could not start)', () => {
    expect(body({ kind: 'offline' })).toContain('The station is offline. The app cannot load.');
    expect(body({ kind: 'gone' })).toContain('This app is no longer stored.');
    const loadError = body({ kind: 'load-error' });
    expect(loadError).toContain('The app could not load.');
    expect(loadError).toContain('Try again');
    const start = body({ kind: 'no-start' });
    expect(start).toContain('The app could not start.');
    expect(start).toContain('Try again');
    const left = body({ kind: 'left' });
    expect(left).toContain('The app left its page and was stopped.');
    expect(left).toContain('Reload app');
    expect(body({ kind: 'collapsed' })).toContain('Show app');
    expect(body({ kind: 'loading' })).toContain('Loading the app…');
  });

  test('a view error maps to a state', () => {
    expect(stateOfViewError(new ApiError(409, 'm', 'apps_unsupported'))).toBeNull();
    expect(stateOfViewError(new ApiError(409, 'm', 'station_unreachable'))).toEqual({
      kind: 'offline',
    });
    expect(stateOfViewError(new ApiError(410, 'm', 'app_gone'))).toEqual({ kind: 'gone' });
    expect(stateOfViewError(new ApiError(502, 'm', 'station_error'))).toEqual({
      kind: 'load-error',
    });
    expect(stateOfViewError(new Error('network'))).toEqual({ kind: 'load-error' });
  });

  test('a closed bridge maps to a state', () => {
    expect(stateOfClose('left')).toEqual({ kind: 'left' });
    expect(stateOfClose('no-hello')).toEqual({ kind: 'no-start' });
    expect(stateOfClose('teardown')).toEqual({ kind: 'collapsed' });
  });
});

describe('open-calls strip', () => {
  test('one line per open call with the text of its state and cause', () => {
    const calls = [
      call({ call_id: '1' }),
      call({ call_id: '2', state: 'uncertain', cause: 'station_restart' }),
      call({ call_id: '3', state: 'uncertain', cause: 'provider_error' }),
    ];
    const out = html(<CallLines calls={calls} session="ses-1" onRelease={() => {}} />);
    expect(out).toContain(
      'An app action is running: bump from demo. A revert of this folder waits until it ends.',
    );
    expect(out).toContain(
      'An app action may still be running after a station restart: bump from demo. A revert of this folder waits until it ends.',
    );
    expect(out).toContain(
      'The provider stopped waiting for an app action, but the tool may still run: bump from demo. A revert of this folder waits until it ends.',
    );
    expect(out.match(/It has stopped/g)).toHaveLength(2);
  });

  test('It has stopped asks, then releases with the call own session', async () => {
    const dialog = html(
      <ReleaseDialog
        call={call({ state: 'uncertain', cause: 'provider_error' })}
        notice={null}
        busy={false}
        onCancel={() => {}}
        onConfirm={() => {}}
      />,
    );
    expect(dialog).toContain('Mark this app action as stopped?');
    expect(dialog).toContain(
      'Only do this when you know that bump is not running. A revert can then change the files that it writes.',
    );
    expect(dialog).toContain('Cancel');
    expect(dialog).toContain('Mark as stopped');
    const posted: string[][] = [];
    const a = actions({
      release: async (station, session, id) => {
        posted.push([station, session, id]);
        return { released: true };
      },
    });
    const result = await releaseCall(a, 'st-1', call({ session_id: 'ses-2', call_id: 'C1' }));
    expect(result).toEqual({ kind: 'done' });
    expect(posted).toEqual([['st-1', 'ses-2', 'C1']]);
  });

  test('released false goes away without a message, call_running has its notice', async () => {
    const none = actions({ release: async () => ({ released: false }) });
    expect(await releaseCall(none, 'st-1', call())).toEqual({ kind: 'done' });
    const running = actions({
      release: async () => {
        throw new ApiError(409, 'busy', 'call_running');
      },
    });
    expect(await releaseCall(running, 'st-1', call())).toEqual({
      kind: 'running',
      text: 'This app action is still running. Stop the session to end it.',
    });
    expect(STILL_RUNNING).toBe('This app action is still running. Stop the session to end it.');
    const other = actions({
      release: async () => {
        throw new Error('boom');
      },
    });
    expect(await releaseCall(other, 'st-1', call())).toEqual({ kind: 'failed', text: 'boom' });
  });

  test('the strip lists a call of a gone app and releases it with the call own session', async () => {
    // The line needs no app row: it reads only the call (`app_id` can name a deleted app).
    const gone = call({ app_id: '01JGONE00000000000000000000', session_id: 'ses-1' });
    expect(callLine(gone, 'ses-1').text).toContain('bump from demo');
    const posted: string[] = [];
    const a = actions({
      release: async (_st, session) => {
        posted.push(session);
        return { released: true };
      },
    });
    await releaseCall(a, 'st-1', call({ session_id: 'ses-9' }));
    expect(posted).toEqual(['ses-9']);
  });

  test('the strip marks a call of another session in the folder', () => {
    const here = callLine(call({ session_id: 'ses-1' }), 'ses-1');
    const there = callLine(call({ session_id: 'ses-2' }), 'ses-1');
    expect(here.text.endsWith(OTHER_SESSION)).toBe(false);
    expect(there.text.endsWith(' It belongs to another session in this folder.')).toBe(true);
  });

  test('the strip asks again when revertKey changes and when refreshCalls runs', () => {
    const admitted = { revert_id: 'r1', turn_id: 't1', state: 'admitted', reason: null } as const;
    const base = summary('ses-1', 'working', 1, { revert: admitted });
    const session = (s: typeof base): DeckSession => ({
      station_id: 'st-1',
      status: 'working',
      summary: s,
    });
    registerAppActions(actions());
    try {
      const a = appsEnvOf(session(base), null, null);
      const b = appsEnvOf(
        session({ ...base, revert: { ...admitted, state: 'succeeded' } }),
        null,
        null,
      );
      expect(a?.revertKey).toBe('r1:admitted:null');
      expect(b?.revertKey).toBe('r1:succeeded:null');
      expect(appsEnvOf(session({ ...base, revert: null }), null, null)?.revertKey).toBeNull();
      const before = callsVersion('st-1', 'ses-1');
      let told = 0;
      const stop = subscribeCalls('st-1', 'ses-1', () => {
        told += 1;
      });
      a?.refreshCalls();
      refreshCalls('st-9', 'ses-9');
      stop();
      a?.refreshCalls();
      expect(told).toBe(1);
      expect(callsVersion('st-1', 'ses-1')).toBe(before + 2);
      expect(callsVersion('st-9', 'ses-9')).toBe(1);
    } finally {
      registerAppActions(null);
    }
  });

  test('appsEnvOf gives owner true for no guest and nothing without registered actions', () => {
    const s: DeckSession = {
      station_id: 'st-1',
      status: 'working',
      summary: summary('ses-1', 'working', 1),
    };
    expect(appsEnvOf(s, null, null)).toBeUndefined();
    registerAppActions(actions());
    try {
      expect(appsEnvOf(s, null, null)?.owner).toBe(true);
      expect(appsEnvOf(s, null, { name: 'guest' })?.owner).toBe(false);
    } finally {
      registerAppActions(null);
    }
  });

  test('the strip renders nothing for an empty list and for a member', () => {
    expect(html(<CallLines calls={[]} session="ses-1" onRelease={() => {}} />)).toBe('');
    expect(html(<AppCalls env={env({ owner: false })} />)).toBe('');
    expect(html(<AppCalls env={env()} />)).toBe('');
  });
});
