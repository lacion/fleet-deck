// Framework-free token state and auth helpers.
//
// Keep this module free of React so transport code and its root-level tests can
// use the board's real authentication contract without requiring the board's
// dependency tree. token.ts is the thin React subscription adapter.
//
// Loopback needs no token. From anywhere else the daemon demands one:
// `Authorization: Bearer <token>` on API calls and `?t=<token>` on WebSockets.
// We adopt that query token once at boot, persist it, and scrub it from the URL.
// The unauthorized flag is a latch: concurrent 401s produce one failure state,
// while attempts lets the token gate distinguish a missing key from a bad one.

import { wsBase } from './base.ts';
import { storageGet, storageRemove, storageSet } from './storage.ts';

export interface AuthState {
  token: string | null;
  unauthorized: boolean;
  attempts: number;
}

const KEY = 'fleetdeck.token';

let state: AuthState = { token: null, unauthorized: false, attempts: 0 };
let booted = false;
const subs = new Set<() => void>();

const emit = () => {
  for (const fn of subs) fn();
};

function writeStored(t: string | null): void {
  if (t) storageSet(KEY, t);
  else storageRemove(KEY);
}

/** Boot: adopt ?t=… (then scrub it from the URL), else fall back to storage. */
export function initToken(): void {
  if (booted) return;
  booted = true;
  let fromUrl: string | null = null;
  try {
    const params = new URLSearchParams(window.location.search);
    fromUrl = (params.get('t') ?? '').trim() || null;
    if (params.has('t')) {
      params.delete('t');
      const qs = params.toString();
      window.history.replaceState(
        null,
        '',
        window.location.pathname + (qs ? `?${qs}` : '') + window.location.hash,
      );
    }
  } catch {
    /* no URL API / blocked history — fall through to storage */
  }

  if (fromUrl) writeStored(fromUrl);
  state = { token: fromUrl ?? storageGet(KEY), unauthorized: false, attempts: 0 };
}

function getToken(): string | null {
  if (!booted) initToken(); // a request before render() must still be armed
  return state.token;
}

/** Latch the "this board needs a token" state (idempotent per failure). */
export function markUnauthorized(): void {
  if (state.unauthorized) return;
  state = { ...state, unauthorized: true, attempts: state.attempts + 1 };
  emit();
}

/** Store a pasted/linked token and clear the failure state so callers retry. */
export function saveToken(raw: string | null | undefined): void {
  const token = (raw ?? '').trim() || null;
  writeStored(token);
  state = { ...state, token, unauthorized: false };
  emit();
}

/** Does this board hold a key at all? Lets a caller tell "the daemon refused
 *  me" apart from "I never had anything to present" — /ws/term is gated even on
 *  loopback, and a refused upgrade is indistinguishable from a dead network. */
export function hasToken(): boolean {
  return !!getToken();
}

/** Authorization header when a token is known — loopback simply has none. */
export function authHeaders(base?: Record<string, string>): Record<string, string> {
  const t = getToken();
  return t ? { ...base, authorization: `Bearer ${t}` } : { ...base };
}

/** ws(s):// URL for `path`, carrying the token in the query string. */
export function wsUrl(
  path: string,
  params?: Record<string, string | number | null | undefined>,
): string {
  // Resolved against the board's own base, not the page root — see base.js.
  const url = wsBase(path);
  for (const [k, v] of Object.entries(params ?? {})) {
    if (v != null) url.searchParams.set(k, String(v));
  }
  const t = getToken();
  if (t) url.searchParams.set('t', t);
  return url.toString();
}

export function subscribeAuth(fn: () => void): () => void {
  subs.add(fn);
  return () => subs.delete(fn);
}

export function authSnapshot(): AuthState {
  return state;
}
