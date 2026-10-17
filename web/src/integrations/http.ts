// R3 (DL-164): `PUT` and `DELETE` for the project routes. `Connection` has `get` and `post` only.
import { ApiError, type Fetch } from '../runtime/api';

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** The error of a refused call: the status, the message, the `error` code and the `field`. */
async function failure(response: Response): Promise<ApiError> {
  let message = `Request failed (${response.status})`;
  let code: string | null = null;
  let field: string | null = null;
  try {
    const value: unknown = await response.json();
    if (record(value)) {
      if (typeof value['message'] === 'string') message = value['message'];
      if (typeof value['error'] === 'string') code = value['error'];
      if (typeof value['field'] === 'string') field = value['field'];
    }
  } catch {
    // The body is not JSON: the status text stays.
  }
  return new ApiError(response.status, message, code, field);
}

export type IntegrationHttp = {
  put(path: string, body: unknown): Promise<unknown>;
  del(path: string): Promise<unknown>;
};

/**
 * `PUT` and `DELETE` with the CSRF token of the login, read when the call is made. The same
 * rules as `artifactHttp`: the cookie, no cache, no redirect, one time limit.
 */
export function integrationHttp(
  fetcher: Fetch,
  csrf: () => string | null,
  timeoutMs: number,
): IntegrationHttp {
  const call = async (path: string, method: 'PUT' | 'DELETE', body?: unknown): Promise<unknown> => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      const token = csrf();
      const headers: Record<string, string> = {};
      if (body !== undefined) headers['Content-Type'] = 'application/json';
      if (token !== null) headers['X-CSRF-Token'] = token;
      const response = await fetcher(path, {
        method,
        credentials: 'same-origin',
        cache: 'no-store',
        redirect: 'error',
        headers,
        signal: controller.signal,
        body: body === undefined ? null : JSON.stringify(body),
      });
      if (!response.ok) throw await failure(response);
      return response.status === 204 ? null : response.json();
    } finally {
      clearTimeout(timer);
    }
  };
  return {
    put: (path, body) => call(path, 'PUT', body),
    del: (path) => call(path, 'DELETE'),
  };
}
