import type { AuthView } from './state';

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}
export type Fetch = (path: string, init: RequestInit) => Promise<Response>;

/** Requests never retry here. Only Connection retries safe session reads. */
export async function request(
  fetcher: Fetch,
  path: string,
  signal: AbortSignal,
  body?: unknown,
  csrf?: string | null,
): Promise<unknown> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (csrf) headers['X-CSRF-Token'] = csrf;
  const response = await fetcher(path, {
    method: body === undefined ? 'GET' : 'POST',
    credentials: 'same-origin',
    cache: 'no-store',
    redirect: 'error',
    headers,
    signal,
    body: body === undefined ? null : JSON.stringify(body),
  });
  if (response.status === 204) return null;
  const value: unknown = await response.json();
  if (!response.ok) {
    const message =
      typeof value === 'object' &&
      value !== null &&
      'message' in value &&
      typeof value.message === 'string'
        ? value.message
        : `Request failed (${response.status})`;
    throw new ApiError(response.status, message);
  }
  return value;
}

/** Auth belongs to the deck API, not a provider credential or browser storage. */
export function authView(value: unknown): AuthView {
  if (
    typeof value !== 'object' ||
    value === null ||
    !('actor' in value) ||
    typeof value.actor !== 'string' ||
    !('scopes' in value) ||
    !Array.isArray(value.scopes) ||
    !value.scopes.every(
      (s: unknown) => typeof s === 'string' && ['read', 'drive', 'approve', 'admin'].includes(s),
    ) ||
    !('csrf' in value) ||
    !(value.csrf === null || typeof value.csrf === 'string') ||
    !('expires_at' in value) ||
    typeof value.expires_at !== 'number' ||
    !Number.isSafeInteger(value.expires_at) ||
    value.expires_at < 0
  ) {
    throw new Error('Invalid login response');
  }
  return value as AuthView;
}

/** Strip a one-time login secret before a request, render, or navigation. */
export function takeLoginLink(
  location: Pick<Location, 'hash' | 'pathname' | 'search'>,
  history: Pick<History, 'replaceState'>,
): string | null {
  const params = new URLSearchParams(location.hash.slice(1));
  const token = params.get('login');
  if (token !== null) history.replaceState(null, '', `${location.pathname}${location.search}`);
  return token;
}
