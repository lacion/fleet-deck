// R11.4 (DL-166): the join page's calls and texts. The invite token is in the URL hash
// (`#join=<token>`); `POST /api/auth/join` redeems it and sets the member's cookie.
import type { ShareRole } from '../gen';

export const MAX_DISPLAY_NAME_CHARS = 40;

/** The invite token of a location hash, or `null` when the hash is not a join link. */
export function joinToken(hash: string): string | null {
  if (!hash.startsWith('#join=')) return null;
  let token: string;
  try {
    token = decodeURIComponent(hash.slice('#join='.length));
  } catch {
    return null;
  }
  return token === '' ? null : token;
}

/** The display name as the deck wants it (1 to 40 characters), or `null`. */
export function displayName(text: string): string | null {
  const name = text.trim();
  const length = [...name].length;
  return length >= 1 && length <= MAX_DISPLAY_NAME_CHARS ? name : null;
}

export const JOIN_ERRORS = {
  invite: 'This invite link is not valid any more. Ask for a new one.',
  tries: 'Too many tries. Wait a minute.',
  name: 'Use 1 to 40 letters, digits or spaces.',
  taken: 'Someone on this deck already uses that name. Pick another one.',
} as const;

export type Joined = { readonly role: ShareRole; readonly display_name: string };
export type JoinResult =
  | { readonly ok: true; readonly joined: Joined | null }
  | { readonly ok: false; readonly text: string };

type Fetch = (path: string, init: RequestInit) => Promise<Response>;

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** The text for a failed join: the three known errors, else the deck's own message. */
export function joinErrorText(status: number, code: string | null, message: string | null): string {
  if (status === 401 && (code === null || code === 'invalid_invite')) return JOIN_ERRORS.invite;
  if (status === 429) return JOIN_ERRORS.tries;
  if (status === 400 && code === 'invalid_display_name') return JOIN_ERRORS.name;
  if (status === 409 && code === 'display_name_taken') return JOIN_ERRORS.taken;
  return message ?? `The deck refused the request (${status}).`;
}

/** Redeems the invite. The answer of a deck with the member fields is kept for `sessionStorage`. */
export async function join(
  fetcher: Fetch,
  token: string,
  name: string,
  signal?: AbortSignal,
): Promise<JoinResult> {
  let response: Response;
  try {
    response = await fetcher('/api/auth/join', {
      method: 'POST',
      credentials: 'same-origin',
      cache: 'no-store',
      redirect: 'error',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ token, display_name: name }),
      ...(signal === undefined ? {} : { signal }),
    });
  } catch {
    return { ok: false, text: 'The deck did not answer. Check the connection and try again.' };
  }
  let value: unknown = null;
  try {
    value = await response.json();
  } catch {
    // An answer without a body: the status decides.
  }
  if (!response.ok) {
    const code = record(value) && typeof value['error'] === 'string' ? value['error'] : null;
    const message = record(value) && typeof value['message'] === 'string' ? value['message'] : null;
    return { ok: false, text: joinErrorText(response.status, code, message) };
  }
  if (
    record(value) &&
    typeof value['role'] === 'string' &&
    typeof value['display_name'] === 'string'
  ) {
    const role = value['role'] as ShareRole;
    return { ok: true, joined: { role, display_name: value['display_name'] } };
  }
  return { ok: true, joined: null };
}
