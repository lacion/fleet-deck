// tests/helpers/http.mjs — thin fetch wrappers for hitting a running daemon.

import http from 'node:http';

import { scaleMs } from './wait.mjs';

/** Deadline-aware raw request for cases fetch() cannot express: a forged
 *  Host/Origin header (undici drops the override), a hook POST with NO helper
 *  token attached. node:http honours the headers exactly as given, but unlike
 *  fetch there is no AbortSignal — so without an explicit deadline a route
 *  that accepts the connection but never answers would hang the whole test
 *  process until the outer CI timeout. The scaled timer destroys the request
 *  and rejects with route diagnostics instead. Never rejects on a non-2xx. */
export function rawRequest({ port, path, method = 'GET', headers = {}, body = null, timeout = 5000, host = '127.0.0.1' }) {
  return new Promise((resolve, reject) => {
    const req = http.request({ host, port, path, method, headers }, res => {
      let text = '';
      res.on('data', chunk => { text += chunk; });
      res.on('end', () => resolve({ status: res.statusCode, text }));
    });
    req.setTimeout(scaleMs(timeout), () => req.destroy(new Error(`raw ${method} ${path} timed out`)));
    req.on('error', reject);
    req.end(body);
  });
}

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
