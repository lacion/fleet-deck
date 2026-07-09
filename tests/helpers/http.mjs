// tests/helpers/http.mjs — thin fetch wrappers for hitting a running daemon.

export async function postJson(url, body, { timeout = 5000 } = {}) {
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
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

export async function getJson(url, { timeout = 5000 } = {}) {
  const res = await fetch(url, { signal: AbortSignal.timeout(timeout) });
  const text = await res.text();
  let json = null;
  if (text) {
    try { json = JSON.parse(text); } catch { /* leave null */ }
  }
  return { status: res.status, json, text };
}

/** POST a hook payload to <baseUrl>/hook/<Event> and return the parsed response body. */
export async function postHook(baseUrl, event, payload, opts) {
  return postJson(`${baseUrl}/hook/${event}`, payload, opts);
}
