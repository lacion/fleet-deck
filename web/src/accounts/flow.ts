// DL-173: what happens in the Accounts tab: a device login, its polling, the OAuth finish, and the
// two relays (an app, a token). No function keeps a secret: a value is read, the input is cleared,
// and the value goes into one request.
import type { AccountKind, AccountView, AppView, ForgeLogin, ForgeLoginState } from '../gen';
import { DeckError, describe } from '../settings/http';
import type { AccountActions } from './actions';

/** How often the board asks for the state of a device login. */
export const POLL_MS = 2_000;

const FORGE_PROBLEMS: Readonly<Record<string, { readonly text: string; readonly token: boolean }>> =
  {
    env_token_present: {
      text: 'A token in the station environment overrides the login. Remove it from the station’s environment, then try again.',
      token: false,
    },
    device_flow_unsupported: {
      text: 'This CLI or this server has no device login.',
      token: true,
    },
    unexpected_cli_output: {
      text: 'The CLI answered in a form that Fleet Deck does not accept. Update the CLI, or use a token.',
      token: true,
    },
  };

/** The sentence for a login problem code, and whether "Use a token instead" fits. */
export function loginProblem(
  code: string | null,
  fallback: string,
): { text: string; token: boolean } {
  const known = code === null ? undefined : FORGE_PROBLEMS[code];
  return known ?? { text: fallback, token: false };
}

export type ForgeStart =
  | { readonly kind: 'started'; readonly login: ForgeLogin }
  | {
      readonly kind: 'error';
      readonly code: string | null;
      readonly text: string;
      readonly token: boolean;
    };

/** Starts a device login on the station. */
export async function startForgeLogin(
  actions: AccountActions,
  station: string,
  kind: AccountKind,
  host: string,
): Promise<ForgeStart> {
  try {
    return { kind: 'started', login: await actions.forgeLogin(station, kind, host.trim()) };
  } catch (e) {
    const code = e instanceof DeckError ? e.code : null;
    return { kind: 'error', code, ...loginProblem(code, describe(e)) };
  }
}

/** The state of a login on the board: the polled state, or a failure to read it. */
export type LoginView = ForgeLoginState;

/** True when the login has ended and the polling stops. */
export const ended = (state: LoginView): boolean => state.state !== 'pending';

/**
 * Asks for the state of a login every `POLL_MS` until it is done, failed or expired, or until
 * `signal` aborts. Three reads in a row that fail end it with `failed`.
 */
export async function pollForgeLogin(
  actions: AccountActions,
  station: string,
  loginId: string,
  opts: {
    readonly wait: (ms: number) => Promise<void>;
    readonly signal: AbortSignal;
    readonly onState: (state: LoginView) => void;
  },
): Promise<void> {
  let misses = 0;
  while (!opts.signal.aborted) {
    await opts.wait(POLL_MS);
    if (opts.signal.aborted) return;
    try {
      const state = await actions.forgeLoginStatus(station, loginId);
      misses = 0;
      if (opts.signal.aborted) return;
      opts.onState(state);
      if (ended(state)) return;
    } catch {
      misses += 1;
      if (misses >= 3) {
        opts.onState({ state: 'failed', code: 'status_unreadable' });
        return;
      }
    }
  }
}

/** The sentence for the end of a login that did not work. */
export function endedText(state: LoginView): string | null {
  if (state.state === 'expired') return 'The code expired. Start the login again.';
  if (state.state === 'failed') {
    if (state.code === 'status_unreadable')
      return 'The board could not read the state of the login.';
    return loginProblem(state.code, `The login failed (${state.code}).`).text;
  }
  return null;
}

// --- OAuth ---

/** The sentence for `?oauth_error=<code>`. */
export const oauthErrorText = (code: string): string =>
  `The login did not finish (${code}). Try again.`;

/** "Connected as ana (acme.atlassian.net)". */
export function connectedText(account: Pick<AccountView, 'user' | 'site'>): string {
  const who = account.user ?? 'your account';
  return account.site === undefined
    ? `Connected as ${who}`
    : `Connected as ${who} (${account.site})`;
}

const claimed = new Set<string>();

/** True the first time a transaction is seen: `?finish=` is posted once, also when the page renders twice. */
export function claimFinish(transaction: string): boolean {
  if (claimed.has(transaction)) return false;
  claimed.add(transaction);
  return true;
}

export type Finished =
  | { readonly ok: true; readonly account: AccountView }
  | { readonly ok: false; readonly text: string };

export async function finishOauth(actions: AccountActions, transaction: string): Promise<Finished> {
  try {
    return { ok: true, account: await actions.oauthFinish(transaction) };
  } catch (e) {
    return { ok: false, text: describe(e) };
  }
}

// --- the relays ---

/** The typed secret: read once, then the input is cleared. */
export type SecretRead = { readonly read: () => string; readonly clear: () => void };

export type Sent<T> =
  | { readonly ok: true; readonly value: T }
  | { readonly ok: false; readonly text: string };

/** Sends the OAuth app of a tracker. The input of the secret is cleared before the call starts. */
export async function sendApp(
  actions: AccountActions,
  station: string,
  kind: AccountKind,
  clientId: string,
  secret: SecretRead,
): Promise<Sent<AppView>> {
  const clientSecret = secret.read();
  secret.clear();
  try {
    const value = await actions.setApp(station, {
      kind,
      client_id: clientId.trim(),
      client_secret: clientSecret,
    });
    return { ok: true, value };
  } catch (e) {
    return { ok: false, text: describe(e) };
  }
}

/** Sends an API token. The input of the token is cleared before the call starts. */
export async function sendToken(
  actions: AccountActions,
  station: string,
  base: { readonly kind: AccountKind; readonly site?: string; readonly email?: string },
  secret: SecretRead,
): Promise<Sent<AccountView>> {
  const token = secret.read();
  secret.clear();
  try {
    const value = await actions.setToken(station, {
      kind: base.kind,
      ...(base.site === undefined || base.site.trim() === '' ? {} : { site: base.site.trim() }),
      ...(base.email === undefined || base.email.trim() === '' ? {} : { email: base.email.trim() }),
      token,
    });
    return { ok: true, value };
  } catch (e) {
    return { ok: false, text: describe(e) };
  }
}

/** The redirect URL of the OAuth app: the board's origin and `/oauth/callback`. */
export const redirectUrlOf = (origin: string): string => `${origin}/oauth/callback`;

export const KIND_LABEL: Readonly<Record<AccountKind, string>> = {
  github: 'GitHub',
  gitlab: 'GitLab',
  jira: 'Jira',
  linear: 'Linear',
};

/** The steps that the owner follows to register the OAuth app (DL-173 design 2.3). */
export function appSteps(kind: 'jira' | 'linear', redirectUrl: string): readonly string[] {
  return kind === 'jira'
    ? [
        'Open developer.atlassian.com, then Console, then Create, then OAuth 2.0 integration.',
        'Add the permission Jira API with the scopes read:jira-work, write:jira-work, read:jira-user; offline access is added by Fleet Deck.',
        `Set the callback URL to ${redirectUrl}.`,
        'Paste the client id and the secret here.',
      ]
    : [
        'Open linear.app, then Settings, then API, then OAuth applications, then New.',
        `Set the callback URL to ${redirectUrl}.`,
        'Paste the client id and the secret here.',
      ];
}
