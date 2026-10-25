// B15 (design `docs/v2/design/mcp-apps.md` §4.7, §4.8): the calls of the app host to the deck. They
// go through `deckHttp`, so the two POSTs carry `X-CSRF-Token` of the cookie login. Every answer is
// checked by shape, in the style of `settings/desktopTools.ts`.
import type {
  AppCallReply,
  AppCallsReply,
  AppReleaseReply,
  AppRequest,
  AppViewReply,
} from '../gen';
import type { DeckIo } from '../settings/http';

/** The calls of the app host. `station` is the station ID, `session` a session ID. */
export type AppActions = {
  /** `GET /api/sessions/{station}/{session}/apps/{app_id}`. */
  view(station: string, session: string, appId: string): Promise<AppViewReply>;
  /** `POST /api/sessions/{station}/{session}/apps/{app_id}/call`. */
  call(
    station: string,
    session: string,
    appId: string,
    request: AppRequest,
    confirmed: boolean,
  ): Promise<AppCallReply>;
  /** `GET /api/sessions/{station}/{session}/app-calls`: the open calls of the session and folder. */
  calls(station: string, session: string): Promise<AppCallsReply>;
  /** `POST /api/sessions/{station}/{callSession}/app-calls/{callId}/release`. */
  release(station: string, callSession: string, callId: string): Promise<AppReleaseReply>;
};

const INVALID = 'The deck sent an invalid answer.';
const bad = (): never => {
  throw new Error(INVALID);
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);
const isStr = (v: unknown): v is string => typeof v === 'string';
const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);
const isStrs = (v: unknown): v is string[] => Array.isArray(v) && v.every(isStr);

/** The answer of the view route, checked by shape. */
export function viewOf(v: unknown): AppViewReply {
  if (
    !record(v) ||
    !isStr(v['url']) ||
    !isNum(v['expires_at']) ||
    !isStr(v['server']) ||
    !isStr(v['tool']) ||
    !isStr(v['resource_uri']) ||
    !record(v['csp']) ||
    !isStrs(v['csp']['connect_domains']) ||
    !isStrs(v['csp']['resource_domains']) ||
    !(v['prefers_border'] === null || typeof v['prefers_border'] === 'boolean')
  ) {
    return bad();
  }
  return v as AppViewReply;
}

/** The answer of the call route, checked by shape. */
export function callReplyOf(v: unknown): AppCallReply {
  if (!record(v)) return bad();
  if (v['outcome'] === 'done' && 'result' in v) return v as AppCallReply;
  if (
    v['outcome'] === 'confirm' &&
    isStr(v['tool']) &&
    (v['title'] === null || isStr(v['title']))
  ) {
    return v as AppCallReply;
  }
  return bad();
}

const STATES = ['running', 'uncertain'];
const CAUSES = ['station_restart', 'provider_error'];

/** The answer of the list route, checked by shape. */
export function callsOf(v: unknown): AppCallsReply {
  if (!record(v) || !Array.isArray(v['calls'])) return bad();
  for (const c of v['calls']) {
    if (
      !record(c) ||
      !isStr(c['call_id']) ||
      !isStr(c['session_id']) ||
      !isStr(c['app_id']) ||
      !isStr(c['server']) ||
      !isStr(c['tool']) ||
      !isStr(c['state']) ||
      !STATES.includes(c['state']) ||
      !(c['cause'] === undefined || (isStr(c['cause']) && CAUSES.includes(c['cause']))) ||
      !isNum(c['created_at'])
    ) {
      return bad();
    }
  }
  return v as AppCallsReply;
}

/** The answer of the release route, checked by shape. */
export function releaseOf(v: unknown): AppReleaseReply {
  if (!record(v) || typeof v['released'] !== 'boolean') return bad();
  return v as AppReleaseReply;
}

const seg = encodeURIComponent;
const sessionBase = (station: string, session: string): string =>
  `/api/sessions/${seg(station)}/${seg(session)}`;

/** The calls over the transport of the settings pages (`deckHttp`). */
export function appActions(io: DeckIo): AppActions {
  return {
    view: async (station, session, appId) =>
      viewOf(await io.get(`${sessionBase(station, session)}/apps/${seg(appId)}`)),
    call: async (station, session, appId, request, confirmed) =>
      callReplyOf(
        await io.post(`${sessionBase(station, session)}/apps/${seg(appId)}/call`, {
          request,
          confirmed,
        }),
      ),
    calls: async (station, session) =>
      callsOf(await io.get(`${sessionBase(station, session)}/app-calls`)),
    release: async (station, callSession, callId) =>
      releaseOf(
        await io.post(
          `${sessionBase(station, callSession)}/app-calls/${seg(callId)}/release`,
          undefined,
        ),
      ),
  };
}
