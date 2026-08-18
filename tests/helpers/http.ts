// tests/helpers/http.ts — thin fetch wrappers for hitting a running daemon.

import http from 'node:http';
import { WebSocket } from 'ws';

import { scaleMs } from './wait.ts';

const hookBoardClients = new Map<string, Promise<WebSocket>>();

/** Open one authenticated snapshot client. Hold-relay tests opt into one via
 * postHook({ boardClient: true }); unrelated daemon tests retain the production
 * default of zero board clients. */
export function connectBoardClient(baseUrl: string, token: string | null): Promise<WebSocket> {
  const url = new URL(baseUrl.replace(/^http/, 'ws') + '/ws');
  if (token) url.searchParams.set('t', token);
  const ws = new WebSocket(url);
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      ws.terminate();
      reject(new Error(`board websocket at ${url.origin} did not open in time`));
    }, scaleMs(5000));
    ws.once('open', () => {
      clearTimeout(timer);
      resolve(ws);
    });
    ws.once('error', (err) => {
      clearTimeout(timer);
      reject(err);
    });
  });
}

export function closeBoardClient(ws: WebSocket): Promise<void> {
  if (ws.readyState === WebSocket.CLOSED) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      try {
        ws.terminate();
      } catch {
        /* already gone */
      }
      resolve();
    }, scaleMs(2000));
    ws.once('close', () => {
      clearTimeout(timer);
      resolve();
    });
    try {
      ws.close();
    } catch {
      clearTimeout(timer);
      resolve();
    }
  });
}

async function ensureHookBoardClient(baseUrl: string, token: string): Promise<void> {
  let pending = hookBoardClients.get(baseUrl);
  if (!pending) {
    pending = connectBoardClient(baseUrl, token);
    hookBoardClients.set(baseUrl, pending);
  }
  try {
    const ws = await pending;
    if (ws.readyState !== WebSocket.OPEN) {
      hookBoardClients.delete(baseUrl);
      await ensureHookBoardClient(baseUrl, token);
    }
  } catch (err) {
    hookBoardClients.delete(baseUrl);
    throw err;
  }
}

export async function releaseHookBoardClient(baseUrl: string): Promise<void> {
  const pending = hookBoardClients.get(baseUrl);
  hookBoardClients.delete(baseUrl);
  if (!pending) return;
  try {
    await closeBoardClient(await pending);
  } catch {
    /* failed connection already owns no live socket */
  }
}

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
    let settled = false;
    let timer: ReturnType<typeof setTimeout>;
    // Settle once and always clear the deadline timer, on every exit path, so a
    // stray timer never outlives the request or double-rejects it.
    const finish = (fn: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      fn();
    };
    const req = http.request({ host, port, path, method, headers }, (res) => {
      let text = '';
      res.on('data', (chunk: Buffer) => {
        text += chunk.toString();
      });
      res.on('end', () => {
        finish(() => {
          resolve({ status: res.statusCode, text });
        });
      });
    });
    // node:http's `req.setTimeout(ms, cb)` fires a socket-inactivity callback
    // under Node, but Bun's node:http compat does not surface it — a hung route
    // then blocks to the outer CI timeout, the exact stall BUG-162 fixed. An
    // explicit deadline timer that destroys the request AND rejects behaves
    // identically on both runtimes.
    timer = setTimeout(() => {
      finish(() => {
        req.destroy();
        reject(new Error(`raw ${method} ${path} timed out`));
      });
    }, scaleMs(timeout));
    req.on('error', (e) => {
      finish(() => {
        reject(e);
      });
    });
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
  // Interactive relay tests opt into a lazily attached authorized board.
  // Omit/false to exercise the production default: no board consumer.
  boardClient?: boolean;
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
  if (
    opts?.boardClient === true &&
    typeof token === 'string' &&
    token &&
    (event === 'PermissionRequest' || event === 'Elicitation' || event === 'AskUserQuestion')
  ) {
    await ensureHookBoardClient(baseUrl, token);
  }
  // exactOptionalPropertyTypes: JsonRequestOptions' optional props reject an
  // explicit `undefined`, so build the forwarded options without ever setting a
  // key to undefined — undefined/null token both mean "no bearer" downstream.
  const forward: JsonRequestOptions = { token: token ?? null };
  if (opts?.timeout !== undefined) forward.timeout = opts.timeout;
  return postJson(`${baseUrl}/hook/${event}`, payload, forward);
}
