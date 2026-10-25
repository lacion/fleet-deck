// DL-173: the account calls of the board. The shapes are the deck's
// (`crates/fleetdeck-deck/src/http/{account_routes,account_secrets}.rs`).
import type {
  AccountKind,
  AccountsList,
  AccountView,
  AppSet,
  AppView,
  Containers,
  ForgeLogin,
  ForgeLoginState,
  TokenSet,
} from '../gen';
import type { DeckIo } from '../settings/http';

export type AccountActions = {
  list(station: string): Promise<AccountsList>;
  forgeLogin(station: string, kind: AccountKind, host: string): Promise<ForgeLogin>;
  forgeLoginStatus(station: string, loginId: string): Promise<ForgeLoginState>;
  logout(station: string, accountId: string): Promise<void>;
  containers(station: string, accountId: string): Promise<Containers>;
  /** The OAuth client id and secret cross the deck once; neither is kept. */
  setApp(station: string, app: AppSet): Promise<AppView>;
  /** An API token crosses the deck once; it is not kept. */
  setToken(station: string, token: TokenSet): Promise<AccountView>;
  oauthStart(station: string, kind: AccountKind): Promise<{ readonly authorize_url: string }>;
  oauthFinish(transaction: string): Promise<AccountView>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

function shaped<T>(v: unknown, fields: readonly string[], what: string): T {
  if (!record(v) || fields.some((f) => !(f in v))) {
    throw new Error(`The deck sent an invalid ${what}.`);
  }
  return v as T;
}

const seg = encodeURIComponent;
const base = (station: string): string => `/api/stations/${seg(station)}/accounts`;
const relay = (station: string): string => `/api/secrets/stations/${seg(station)}/accounts`;

const ACCOUNT_FIELDS = ['account_id', 'kind', 'method', 'status'];

/** The account calls over a transport. */
export function accountActions(io: DeckIo): AccountActions {
  return {
    list: async (station) => {
      const v = shaped<AccountsList>(
        await io.get(base(station)),
        ['accounts', 'apps', 'env_tokens'],
        'account list',
      );
      if (!Array.isArray(v.accounts) || !Array.isArray(v.apps) || !Array.isArray(v.env_tokens)) {
        throw new Error('The deck sent an invalid account list.');
      }
      return v;
    },
    forgeLogin: async (station, kind, host) =>
      shaped<ForgeLogin>(
        await io.post(`${base(station)}/forge/login`, { kind, host }),
        ['login_id', 'code', 'url', 'expires_in'],
        'device login',
      ),
    forgeLoginStatus: async (station, loginId) =>
      shaped<ForgeLoginState>(
        await io.get(`${base(station)}/forge/login/${seg(loginId)}`),
        ['state'],
        'login status',
      ),
    logout: async (station, accountId) => {
      await io.post(`${base(station)}/${seg(accountId)}/logout`, {});
    },
    containers: async (station, accountId) => {
      const v = shaped<Containers>(
        await io.get(`${base(station)}/${seg(accountId)}/containers`),
        ['containers'],
        'container list',
      );
      if (!Array.isArray(v.containers)) throw new Error('The deck sent an invalid container list.');
      return v;
    },
    setApp: async (station, app) =>
      shaped<AppView>(
        await io.post(`${relay(station)}/apps`, app),
        ['kind', 'client_id', 'has_secret'],
        'OAuth app',
      ),
    setToken: async (station, token) =>
      shaped<AccountView>(
        await io.post(`${relay(station)}/token`, token),
        ACCOUNT_FIELDS,
        'account',
      ),
    oauthStart: async (station, kind) =>
      shaped<{ authorize_url: string }>(
        await io.post('/api/accounts/oauth/start', { station, kind }),
        ['authorize_url'],
        'sign-in address',
      ),
    oauthFinish: async (transaction) =>
      shaped<AccountView>(
        await io.post('/api/accounts/oauth/finish', { transaction }),
        ACCOUNT_FIELDS,
        'account',
      ),
  };
}
