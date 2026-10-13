// DL-172, DL-173: the transport of the settings and accounts calls. `Connection.post` drops the
// details of an error answer (`keys`, `would_stop`); these pages need them. The rules are the same
// as `integrationHttp`: the cookie, the CSRF token read at call time, no cache, no redirect, one
// time limit. A request body can hold a secret: it is sent and dropped, never stored or logged.
import { ApiError, type Fetch } from '../runtime/api';

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** An error answer of the deck: its code and text, and the other fields of the body. */
export class DeckError extends ApiError {
  constructor(
    status: number,
    message: string,
    code: string | null,
    /** The body fields besides `error` and `message`, for example `keys` or `would_stop`. */
    readonly details: Readonly<Record<string, unknown>>,
  ) {
    super(status, message, code);
  }
}

async function failure(response: Response): Promise<DeckError> {
  let message = `Request failed (${response.status})`;
  let code: string | null = null;
  const details: Record<string, unknown> = {};
  try {
    const value: unknown = await response.json();
    if (record(value)) {
      if (typeof value['message'] === 'string') message = value['message'];
      if (typeof value['error'] === 'string') code = value['error'];
      for (const [k, v] of Object.entries(value)) {
        if (k !== 'error' && k !== 'message') details[k] = v;
      }
    }
  } catch {
    // The body is not JSON: the status text stays.
  }
  return new DeckError(response.status, message, code, details);
}

export type DeckIo = {
  get(path: string): Promise<unknown>;
  post(path: string, body: unknown): Promise<unknown>;
};

/** `GET` and `POST` of the deck API with the details of an error. */
export function deckHttp(fetcher: Fetch, csrf: () => string | null, timeoutMs: number): DeckIo {
  const call = async (path: string, body?: unknown): Promise<unknown> => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      const token = csrf();
      const headers: Record<string, string> = {};
      if (body !== undefined) headers['Content-Type'] = 'application/json';
      if (token !== null) headers['X-CSRF-Token'] = token;
      const response = await fetcher(path, {
        method: body === undefined ? 'GET' : 'POST',
        credentials: 'same-origin',
        cache: 'no-store',
        redirect: 'error',
        headers,
        signal: controller.signal,
        body: body === undefined ? null : JSON.stringify(body),
      });
      if (!response.ok) throw await failure(response);
      return response.status === 204 ? null : await response.json();
    } finally {
      clearTimeout(timer);
    }
  };
  return { get: (path) => call(path), post: (path, body) => call(path, body) };
}

/** The text for a failure: the deck's fixed text, else a plain sentence. */
export function describe(e: unknown): string {
  if (e instanceof DOMException && e.name === 'AbortError') {
    return 'The deck did not answer in time.';
  }
  return e instanceof Error ? e.message : 'The deck did not answer.';
}
