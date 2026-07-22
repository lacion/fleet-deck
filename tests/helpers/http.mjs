// tests/helpers/http.mjs — thin fetch wrappers for hitting a running daemon.

export async function postJson(url, body, { timeout = 5000, token = null } = {}) {
  const headers = { 'content-type': 'application/json' };
  if (token) headers.authorization = `Bearer ${token}`;
  const res = await fetch(url, {
    method: 'POST',
    headers,
    body: JSON.stringify(body ?? {}),
    signal: AbortSignal.timeout(timeout),
  });
  const text = await res.text();
  let json = null;
  if (text) {
    try { json = JSON.parse(text); } catch { /* leave null, caller can inspect text */ }
  }
  return { status: res.status, json, text };
}

export async function getJson(url, { timeout = 5000, token = null } = {}) {
  const headers = token ? { authorization: `Bearer ${token}` } : undefined;
  const res = await fetch(url, { headers, signal: AbortSignal.timeout(timeout) });
  const text = await res.text();
  let json = null;
  if (text) {
    try { json = JSON.parse(text); } catch { /* leave null */ }
  }
  return { status: res.status, json, text };
}

/** POST a hook payload to <baseUrl>/hook/<Event> and return the parsed response body.
 *  Since 0.16.0 hooks require the daemon's bearer; pass the daemon handle (or a
 *  raw token) via { token }. Tests asserting the 401 gate omit it deliberately. */
export async function postHook(baseUrl, event, payload, opts) {
  const token = typeof opts?.token === 'object' && opts.token !== null ? opts.token.token : opts?.token;
  return postJson(`${baseUrl}/hook/${event}`, payload, { ...opts, token });
}
