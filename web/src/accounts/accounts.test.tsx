import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { AccountsList, AccountView } from '../gen';
import { DeckError } from '../settings/http';
import { station } from '../testing/fleet';
import type { AccountActions } from './actions';
import { AppDialog, TokenDialog } from './AccountDialogs';
import { AccountsTab, envTokenText } from './AccountsTab';
import {
  appSteps,
  claimFinish,
  connectedText,
  endedText,
  finishOauth,
  loginProblem,
  oauthErrorText,
  pollForgeLogin,
  redirectUrlOf,
  sendApp,
  sendToken,
  startForgeLogin,
} from './flow';
import { DeviceLoginPanel, ForgeConnect, safeUrl } from './ForgeSection';

const noop = () => {};

const account = (
  extra: Partial<AccountView> & Pick<AccountView, 'account_id' | 'kind'>,
): AccountView => ({ method: 'oauth', status: 'ok', message: '', ...extra });

const LIST: AccountsList = {
  accounts: [
    account({ account_id: 'gh-1', kind: 'github', method: 'cli', site: 'github.com', user: 'ana' }),
    account({
      account_id: 'jira-1',
      kind: 'jira',
      site: 'acme.atlassian.net',
      user: 'ana@acme.test',
      status: 'needs_login',
    }),
  ],
  apps: [{ kind: 'jira', client_id: 'abc123', has_secret: true, generation: 2 }],
  env_tokens: ['GH_TOKEN', 'GITLAB_TOKEN'],
};

type Fake = Partial<AccountActions>;

const DEFAULTS: AccountActions = {
  list: async () => LIST,
  forgeLogin: async () => ({
    login_id: 'l1',
    code: 'ABCD-1234',
    url: 'https://github.com/login/device',
    expires_in: 900,
  }),
  forgeLoginStatus: async () => ({ state: 'pending' as const }),
  logout: async () => {},
  containers: async () => ({ containers: [] }),
  setApp: async () => ({
    kind: 'jira' as const,
    client_id: 'abc',
    has_secret: true,
    generation: 1,
  }),
  setToken: async () => account({ account_id: 't', kind: 'jira' }),
  oauthStart: async () => ({ authorize_url: 'https://auth.atlassian.com/authorize' }),
  oauthFinish: async () =>
    account({ account_id: 'jira-2', kind: 'jira', site: 'acme.atlassian.net', user: 'ana' }),
};

/** Fake actions that record each call (the name and the arguments). */
function fake(over: Fake = {}) {
  const calls: { name: string; arg: unknown }[] = [];
  const merged: Record<string, (...args: unknown[]) => Promise<unknown>> = {};
  for (const [name, fn] of Object.entries({ ...DEFAULTS, ...over })) {
    merged[name] = (...args: unknown[]) => {
      calls.push({ name, arg: args });
      return (fn as (...a: unknown[]) => Promise<unknown>)(...args);
    };
  }
  return { actions: merged as unknown as AccountActions, calls };
}

const tab = (route: Partial<Parameters<typeof AccountsTab>[0]['route']> = {}, list = LIST) =>
  renderToStaticMarkup(
    <AccountsTab
      route={{
        name: 'settings',
        tab: 'accounts',
        station: null,
        finish: null,
        oauthError: null,
        ...route,
      }}
      stations={[station('station-a'), station('station-b')]}
      navigate={noop}
      actions={fake().actions}
      origin="https://deck.example"
      initial={list}
    />,
  );

describe('the Accounts tab (DL-173)', () => {
  test('a station select when there is more than one station', () => {
    const html = tab();
    expect(html).toContain('>Station<');
    expect(html).toContain('<option value="station-a" selected="">Alpha</option>');
    const one = renderToStaticMarkup(
      <AccountsTab
        route={{ name: 'settings', tab: 'accounts', station: null, finish: null, oauthError: null }}
        stations={[station('station-a')]}
        navigate={noop}
        actions={fake().actions}
        initial={LIST}
      />,
    );
    expect(one).not.toContain('>Station<');
  });

  test('the env token warning names the variables', () => {
    expect(envTokenText(['GH_TOKEN'])).toBe(
      'GH_TOKEN in the station environment override a login. Remove them from the station’s environment to use the login here.',
    );
    expect(tab()).toContain('GH_TOKEN, GITLAB_TOKEN in the station environment override a login.');
    expect(tab({}, { ...LIST, env_tokens: [] })).not.toContain('in the station environment');
  });

  test('two sections; the accounts, the host inputs and the connect buttons', () => {
    const html = tab();
    expect(html).toContain('GitHub and GitLab');
    expect(html).toContain('Jira and Linear');
    expect(html).toContain('github.com');
    expect(html).toContain('>Log out<');
    expect(html).toContain('Connect GitHub');
    expect(html).toContain('Connect GitLab');
    expect(html).toContain('value="gitlab.com"');
    expect(html).toContain('OAuth app: client id');
    expect(html).toContain('abc123');
    expect(html).toContain('secret set');
    expect(html).toContain('No OAuth app yet.');
    expect(html).toContain('Set up the OAuth app…');
    expect(html).toContain('Connect Jira');
    expect(html).toContain('Use an API token instead');
    // Linear has no app: its connect button is off.
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Connect Linear<\/button>/);
    expect(html).not.toMatch(/<button[^>]*disabled=""[^>]*>Connect Jira<\/button>/);
  });

  test('"Reconnect" shows for needs_login', () => {
    expect(tab()).toContain('>Reconnect<');
    expect(tab({}, { ...LIST, accounts: [LIST.accounts[0] as AccountView] })).not.toContain(
      '>Reconnect<',
    );
  });

  test('?oauth_error shows the sentence', () => {
    expect(oauthErrorText('invalid_state')).toBe(
      'The login did not finish (invalid_state). Try again.',
    );
    expect(tab({ oauthError: 'invalid_state' })).toContain(
      'The login did not finish (invalid_state). Try again.',
    );
    expect(tab()).not.toContain('The login did not finish');
  });

  test('without a station it says so', () => {
    const html = renderToStaticMarkup(
      <AccountsTab
        route={{ name: 'settings', tab: 'accounts', station: null, finish: null, oauthError: null }}
        stations={[]}
        navigate={noop}
        actions={fake().actions}
      />,
    );
    expect(html).toContain('Add a station first.');
  });
});

describe('the device login', () => {
  test('the panel shows the code, Copy code and a safe link', () => {
    const html = renderToStaticMarkup(
      <DeviceLoginPanel
        kind="github"
        login={{
          login_id: 'l1',
          code: 'ABCD-1234',
          url: 'https://github.com/login/device',
          expires_in: 900,
        }}
        state={{ state: 'pending' }}
      />,
    );
    expect(html).toContain('ABCD-1234');
    expect(html).toContain('Copy code');
    expect(html).toContain('href="https://github.com/login/device"');
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer"');
    expect(html).toContain('Open https://github.com/login/device');
    expect(html).toContain('15 minutes');
    expect(html).toContain('Waiting for you');
  });

  test('a link that is not https is not shown', () => {
    expect(safeUrl('javascript:alert(1)')).toBeNull();
    expect(safeUrl('http://github.com/login/device')).toBeNull();
    expect(safeUrl('https://gitlab.com/oauth/device')).toBe('https://gitlab.com/oauth/device');
    const html = renderToStaticMarkup(
      <DeviceLoginPanel
        kind="github"
        login={{ login_id: 'l', code: 'A', url: 'javascript:alert(1)', expires_in: 60 }}
        state={{ state: 'pending' }}
      />,
    );
    expect(html).not.toContain('href=');
  });

  test('the end states say what happened', () => {
    const login = {
      login_id: 'l',
      code: 'A',
      url: 'https://github.com/login/device',
      expires_in: 60,
    };
    const done = renderToStaticMarkup(
      <DeviceLoginPanel kind="github" login={login} state={{ state: 'done', user: 'ana' }} />,
    );
    expect(done).toContain('Connected as ana.');
    expect(done).not.toContain('Waiting for you');
    const expired = renderToStaticMarkup(
      <DeviceLoginPanel kind="github" login={login} state={{ state: 'expired' }} />,
    );
    expect(expired).toContain('The code expired.');
    expect(endedText({ state: 'failed', code: 'device_flow_unsupported' })).toBe(
      'This CLI or this server has no device login.',
    );
    expect(endedText({ state: 'pending' })).toBeNull();
  });

  test('a running login renders in the connect block', () => {
    const html = renderToStaticMarkup(
      <ForgeConnect
        kind="github"
        station="station-a"
        actions={fake().actions}
        onConnected={noop}
        onToken={noop}
        initialRun={{
          phase: 'running',
          login: {
            login_id: 'l1',
            code: 'WXYZ-9999',
            url: 'https://github.com/login/device',
            expires_in: 900,
          },
          state: { state: 'pending' },
        }}
      />,
    );
    expect(html).toContain('WXYZ-9999');
    const error = renderToStaticMarkup(
      <ForgeConnect
        kind="gitlab"
        station="station-a"
        actions={fake().actions}
        onConnected={noop}
        onToken={noop}
        initialRun={{
          phase: 'error',
          text: 'This CLI or this server has no device login.',
          token: true,
        }}
      />,
    );
    expect(error).toContain('Use a token instead');
  });

  test('the three error codes have a sentence each; only two offer a token', async () => {
    expect(loginProblem('env_token_present', 'x')).toMatchObject({ token: false });
    expect(loginProblem('env_token_present', 'x').text).toContain('overrides the login');
    expect(loginProblem('device_flow_unsupported', 'x').token).toBe(true);
    expect(loginProblem('unexpected_cli_output', 'x').token).toBe(true);
    expect(loginProblem('other', 'fallback')).toEqual({ text: 'fallback', token: false });
    for (const code of ['env_token_present', 'device_flow_unsupported', 'unexpected_cli_output']) {
      const { actions } = fake({
        forgeLogin: () => Promise.reject(new DeckError(409, 'fixed text', code, {})),
      });
      const out = await startForgeLogin(actions, 'st', 'github', 'github.com');
      expect(out).toMatchObject({ kind: 'error', code });
    }
  });

  test('the polling asks every time until the login is done, then stops', async () => {
    const states = [
      { state: 'pending' },
      { state: 'pending' },
      { state: 'done', user: 'ana' },
      { state: 'pending' },
    ] as const;
    let i = 0;
    const { actions, calls } = fake({
      forgeLoginStatus: () => Promise.resolve(states[i++] ?? { state: 'pending' as const }),
    });
    const seen: string[] = [];
    const waits: number[] = [];
    await pollForgeLogin(actions, 'st', 'l1', {
      signal: new AbortController().signal,
      wait: async (ms) => {
        waits.push(ms);
      },
      onState: (s) => seen.push(s.state),
    });
    expect(seen).toEqual(['pending', 'pending', 'done']);
    expect(waits).toEqual([2000, 2000, 2000]);
    expect(calls.filter((c) => c.name === 'forgeLoginStatus')).toHaveLength(3);
  });

  test('failed and expired also stop it; an abort stops it; three misses fail it', async () => {
    for (const end of [{ state: 'failed', code: 'x' }, { state: 'expired' }] as const) {
      const { actions, calls } = fake({ forgeLoginStatus: () => Promise.resolve(end) });
      await pollForgeLogin(actions, 's', 'l', {
        signal: new AbortController().signal,
        wait: async () => {},
        onState: noop,
      });
      expect(calls).toHaveLength(1);
    }
    const controller = new AbortController();
    const { actions, calls } = fake();
    await pollForgeLogin(actions, 's', 'l', {
      signal: controller.signal,
      wait: async () => {
        controller.abort();
      },
      onState: noop,
    });
    expect(calls).toHaveLength(0);
    const down = fake({ forgeLoginStatus: () => Promise.reject(new Error('net')) });
    const seen: string[] = [];
    await pollForgeLogin(down.actions, 's', 'l', {
      signal: new AbortController().signal,
      wait: async () => {},
      onState: (s) => seen.push(s.state),
    });
    expect(seen).toEqual(['failed']);
    expect(down.calls).toHaveLength(3);
  });
});

describe('the OAuth app dialog', () => {
  test('Jira: the four steps with the redirect URL, and Copy', () => {
    const url = redirectUrlOf('https://deck.example');
    expect(url).toBe('https://deck.example/oauth/callback');
    const html = renderToStaticMarkup(
      <AppDialog
        kind="jira"
        redirectUrl={url}
        initialClientId=""
        busy={false}
        error={null}
        onSubmit={noop}
        onCancel={noop}
      />,
    );
    expect(html).toContain('Set up the Jira OAuth app');
    expect(html).toContain(
      'Open developer.atlassian.com, then Console, then Create, then OAuth 2.0 integration.',
    );
    expect(html).toContain(
      'Add the permission Jira API with the scopes read:jira-work, write:jira-work, read:jira-user; offline access is added by Fleet Deck.',
    );
    expect(html).toContain('Set the callback URL to https://deck.example/oauth/callback.');
    expect(html).toContain('Paste the client id and the secret here.');
    expect(html).toContain('Copy</button>');
    expect(html).toContain('type="password"');
    expect(html).toContain('Client id');
    expect(html).toContain('Client secret');
  });

  test('Linear has three steps', () => {
    expect(appSteps('linear', 'https://x/oauth/callback')).toEqual([
      'Open linear.app, then Settings, then API, then OAuth applications, then New.',
      'Set the callback URL to https://x/oauth/callback.',
      'Paste the client id and the secret here.',
    ]);
    expect(appSteps('jira', 'u')).toHaveLength(4);
  });

  test('the token dialog asks for what the kind needs', () => {
    const jira = renderToStaticMarkup(
      <TokenDialog
        kind="jira"
        initialSite=""
        busy={false}
        error={null}
        onSubmit={noop}
        onCancel={noop}
      />,
    );
    expect(jira).toContain('Jira site');
    expect(jira).toContain('Account email');
    expect(jira).toContain('API token');
    const linear = renderToStaticMarkup(
      <TokenDialog
        kind="linear"
        initialSite=""
        busy={false}
        error={null}
        onSubmit={noop}
        onCancel={noop}
      />,
    );
    expect(linear).toContain('Personal API key');
    expect(linear).not.toContain('Account email');
    const github = renderToStaticMarkup(
      <TokenDialog
        kind="github"
        initialSite="github.com"
        busy={false}
        error={null}
        onSubmit={noop}
        onCancel={noop}
      />,
    );
    expect(github).toContain('value="github.com"');
    expect(github).not.toContain('value="ghp');
  });

  test('a secret is cleared before the call, and no result holds it', async () => {
    let typed = 'client-secret-xyz';
    const order: string[] = [];
    const { actions, calls } = fake({
      setApp: () => {
        order.push('call');
        return Promise.resolve({ kind: 'jira', client_id: 'abc', has_secret: true, generation: 3 });
      },
    });
    const out = await sendApp(actions, 'st', 'jira', ' abc ', {
      read: () => typed,
      clear: () => {
        order.push('clear');
        typed = '';
      },
    });
    expect(order).toEqual(['clear', 'call']);
    expect(typed).toBe('');
    expect(out.ok).toBe(true);
    expect(JSON.stringify(out)).not.toContain('client-secret-xyz');
    expect(calls[0]?.arg).toEqual([
      'st',
      { kind: 'jira', client_id: 'abc', client_secret: 'client-secret-xyz' },
    ]);
    let token = 'tok-123';
    const failing = fake({
      setToken: () => Promise.reject(new DeckError(400, 'fixed text', 'invalid_field', {})),
    });
    const bad = await sendToken(
      failing.actions,
      'st',
      { kind: 'jira', site: ' https://a.atlassian.net ', email: '' },
      {
        read: () => token,
        clear: () => {
          token = '';
        },
      },
    );
    expect(bad).toEqual({ ok: false, text: 'fixed text' });
    expect(token).toBe('');
    expect(failing.calls[0]?.arg).toEqual([
      'st',
      { kind: 'jira', site: 'https://a.atlassian.net', token: 'tok-123' },
    ]);
  });
});

describe('the OAuth finish', () => {
  test('a transaction is posted once, also when the page renders twice', () => {
    expect(claimFinish('t-unique-1')).toBe(true);
    expect(claimFinish('t-unique-1')).toBe(false);
    expect(claimFinish('t-unique-2')).toBe(true);
  });

  test('success gives "Connected as <user> (<site>)"', async () => {
    const { actions, calls } = fake();
    const out = await finishOauth(actions, 't-9');
    expect(out.ok).toBe(true);
    expect(calls[0]).toEqual({ name: 'oauthFinish', arg: ['t-9'] });
    if (out.ok) expect(connectedText(out.account)).toBe('Connected as ana (acme.atlassian.net)');
    expect(connectedText({ user: 'ana' })).toBe('Connected as ana');
  });

  test('an error shows the fixed text of the deck', async () => {
    const { actions } = fake({
      oauthFinish: () =>
        Promise.reject(
          new DeckError(
            404,
            'This sign-in is unknown, used or expired.',
            'unknown_transaction',
            {},
          ),
        ),
    });
    expect(await finishOauth(actions, 't')).toEqual({
      ok: false,
      text: 'This sign-in is unknown, used or expired.',
    });
  });
});
