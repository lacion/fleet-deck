// LAN-mode bearer token — the board's half of the daemon's auth contract.
//
// Loopback needs no token (unchanged). From anywhere else the daemon demands
// one: `Authorization: Bearer <token>` on API calls, `?t=<token>` on the
// WebSocket (browsers cannot set headers on a WS handshake). Missing/invalid
// → 401 {ok:false, reason:'unauthorized'}.
//
// The daemon prints http://<host>:4711/?t=<token>. We take that `t` ONCE at
// boot, put it in localStorage, and immediately scrub it from the address bar
// with replaceState — a live credential should not sit in the URL where it
// rides along into bookmarks, screenshots, and shoulder-surfing range.
//
// Store shape: {token, unauthorized, attempts}. `attempts` counts 401s so the
// gate can tell "you need a token" from "that token was wrong" — but the flag
// is a LATCH, not a counter of noise: any number of concurrent 401s produce
// exactly one failure state.

import { useSyncExternalStore } from 'react';
import { wsBase } from './base.ts';

// The board's live auth state, published through useAuth() (useSyncExternalStore).
interface AuthState {
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

function readStored(): string | null {
  try {
    return localStorage.getItem(KEY) ?? null;
  } catch {
    return null;
  } // private mode / blocked storage
}
function writeStored(t: string | null): void {
  try {
    if (t) localStorage.setItem(KEY, t);
    else localStorage.removeItem(KEY);
  } catch {
    /* the in-memory token still carries this tab */
  }
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
  state = { token: fromUrl ?? readStored(), unauthorized: false, attempts: 0 };
}

// in-file only — authHeaders()/wsUrl() below are the exported surface over it.
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

const snapshot = (): AuthState => state;

export function useAuth(): AuthState {
  return useSyncExternalStore((fn) => {
    subs.add(fn);
    return () => subs.delete(fn);
  }, snapshot);
}
