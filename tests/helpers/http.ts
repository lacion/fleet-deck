// tests/helpers/http.ts — thin fetch wrappers for hitting a running daemon.

import http from 'node:http';

import { scaleMs } from './wait.ts';

export interface RawRequestOptions {
  port: number;
  path: string;
  method?: string;
  headers?: http.OutgoingHttpHeaders;
  body?: string | null;
  timeout?: number;
  host?: string;
}

export interface RawResponse {
  status: number | undefined;
  text: string;
}

/** Deadline-aware raw request for cases fetch() cannot express: a forged
 *  Host/Origin header (undici drops the override), a hook POST with NO helper
 *  token attached. node:http honours the headers exactly as given, but unlike
 *  fetch there is no AbortSignal — so without an explicit deadline a route
 *  that accepts the connection but never answers would hang the whole test
 *  process until the outer CI timeout. The scaled timer destroys the request
 *  and rejects with route diagnostics instead. Never rejects on a non-2xx. */
export function rawRequest({
  port,
  path,
  method = 'GET',
  headers = {},
  body = null,
  timeout = 5000,
  host = '127.0.0.1',
}: RawRequestOptions): Promise<RawResponse> {
  return new Promise((resolve, reject) => {
    const req = http.request({ host, port, path, method, headers }, (res) => {
      let text = '';
      res.on('data', (chunk: Buffer) => {
        text += chunk.toString();
      });
      res.on('end', () => {
        resolve({ status: res.statusCode, text });
      });
    });
    req.setTimeout(scaleMs(timeout), () =>
      req.destroy(new Error(`raw ${method} ${path} timed out`)),
    );
    req.on('error', reject);
    req.end(body);
  });
}

export interface JsonRequestOptions {
  timeout?: number;
  token?: string | null;
}

export interface JsonResponse {
  status: number;
  json: unknown;
  text: string;
}

export async function postJson(
  url: string,
  body: unknown,
  { timeout = 5000, token = null }: JsonRequestOptions = {},
): Promise<JsonResponse> {
  const headers: Record<string, string> = { 'content-type': 'application/json' };
  if (token) headers['authorization'] = `Bearer ${token}`;
  const res = await fetch(url, {
    method: 'POST',
    headers,
    body: JSON.stringify(body ?? {}),
    signal: AbortSignal.timeout(timeout),
  });
  const text = await res.text();
  let json: unknown = null;
  if (text) {
    try {
      json = JSON.parse(text);
    } catch {
      /* leave null, caller can inspect text */
    }
  }
  return { status: res.status, json, text };
}

export async function getJson(
  url: string,
  { timeout = 5000, token = null }: JsonRequestOptions = {},
): Promise<JsonResponse> {
  const headers = token ? { authorization: `Bearer ${token}` } : undefined;
  const res = await fetch(url, { headers, signal: AbortSignal.timeout(timeout) });
  const text = await res.text();
  let json: unknown = null;
  if (text) {
    try {
      json = JSON.parse(text);
    } catch {
      /* leave null */
    }
  }
  return { status: res.status, json, text };
}

/** A daemon handle exposes its bearer as `{ token }`; postHook also accepts a
 *  bare token string, or nothing (the tests asserting the 401 gate). */
export interface HookOptions {
  timeout?: number;
  token?: string | { token: string | null } | null;
}

/** POST a hook payload to <baseUrl>/hook/<Event> and return the parsed response body.
 *  Since 0.16.0 hooks require the daemon's bearer; pass the daemon handle (or a
 *  raw token) via { token }. Tests asserting the 401 gate omit it deliberately. */
export async function postHook(
  baseUrl: string,
  event: string,
  payload: unknown,
  opts?: HookOptions,
): Promise<JsonResponse> {
  const token =
    typeof opts?.token === 'object' && opts.token !== null ? opts.token.token : opts?.token;
  // exactOptionalPropertyTypes: JsonRequestOptions' optional props reject an
  // explicit `undefined`, so build the forwarded options without ever setting a
  // key to undefined — undefined/null token both mean "no bearer" downstream.
  const forward: JsonRequestOptions = { token: token ?? null };
  if (opts?.timeout !== undefined) forward.timeout = opts.timeout;
  return postJson(`${baseUrl}/hook/${event}`, payload, forward);
}
