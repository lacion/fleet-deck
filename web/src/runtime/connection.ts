import type { BoardClientMessage, SessionKey } from '../gen';
import { ApiError, authView, type Fetch, request } from './api';
import { clearAuthoringState } from './authoringState';
import { boardMessage } from './decode';
import {
  emptyState,
  emptyTimeline,
  type RuntimeState,
  reduceMessage,
  sessionKey,
  stale,
} from './state';

export type Socket = {
  onopen: (() => void) | null;
  onmessage: ((event: { data: string }) => void) | null;
  onclose: ((event: { code: number }) => void) | null;
  onerror: (() => void) | null;
  send(data: string): void;
  close(): void;
};
type Cancel = () => void;
export type Environment = {
  fetch: Fetch;
  socket: () => Socket;
  schedule: (fn: () => void, ms: number) => Cancel;
  random: () => number;
};
export const IDLE_MS = 5 * 60_000;
export const TIMEOUT_MS = 10_000;
export function browserEnvironment(): Environment {
  return {
    fetch: (path, init) => fetch(path, init),
    socket: () => {
      const url = new URL('/api/ws', window.location.href);
      url.protocol = 'wss:';
      const native = new WebSocket(url);
      const port: Socket = {
        onopen: null,
        onmessage: null,
        onclose: null,
        onerror: null,
        send: (data) => native.send(data),
        close: () => native.close(),
      };
      native.onopen = () => port.onopen?.();
      native.onmessage = (event) => port.onmessage?.({ data: String(event.data) });
      native.onclose = (event) => port.onclose?.({ code: event.code });
      native.onerror = () => port.onerror?.();
      return port;
    },
    schedule: (fn, ms) => {
      const id = setTimeout(fn, ms);
      return () => clearTimeout(id);
    },
    random: Math.random,
  };
}
type Watch = { key: SessionKey; users: number; idle: Cancel | null; pending: Cancel | null };

/** One instance per browser page owns every reconnect and timeline subscription. */
export class Connection {
  private state: RuntimeState = emptyState();
  private listeners = new Set<() => void>();
  private watches = new Map<string, Watch>();
  private socket: Socket | null = null;
  private retry: Cancel | null = null;
  private deadline: Cancel | null = null;
  private stable: Cancel | null = null;
  private abort: AbortController | null = null;
  private generation = 0;
  private attempt = 0;
  private running = false;
  private hello = false;

  constructor(private readonly env: Environment) {}
  getSnapshot = (): RuntimeState => this.state;
  subscribe = (listener: () => void): Cancel => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private update(state: RuntimeState) {
    this.state = state;
    for (const listener of this.listeners) listener();
  }
  start(token: string | null = null): void {
    if (this.running) return;
    this.running = true;
    // NV-05: a link token starts a new login. A cookie session is a reconnect.
    if (token !== null) clearAuthoringState();
    void this.connect(
      token === null ? '/api/auth/session' : '/api/auth/link',
      token === null ? undefined : { token },
    );
  }
  stop(): void {
    this.running = false;
    this.cancelTransport();
    for (const watch of this.watches.values()) {
      watch.idle?.();
      watch.pending?.();
    }
    this.watches.clear();
    this.update({ ...stale(this.state), transport: 'stopped', timelines: {} });
  }
  login(password: string): Promise<void> {
    this.running = true;
    // NV-05: a login starts a new account.
    clearAuthoringState();
    return this.connect('/api/auth/login', { password });
  }
  async logout(): Promise<void> {
    // NV-05: drafts and legacy notes do not outlive the login.
    clearAuthoringState();
    const csrf = this.state.login?.csrf;
    this.cancelTransport();
    const gen = this.generation;
    const controller = new AbortController();
    this.abort = controller;
    const cancel = this.env.schedule(() => controller.abort(), TIMEOUT_MS);
    try {
      await request(this.env.fetch, '/api/auth/logout', controller.signal, {}, csrf);
    } finally {
      cancel();
      if (gen === this.generation) this.anonymous(null);
    }
  }
  /** One read call (no body, no CSRF token). It never retries. */
  async get(path: string, timeoutMs: number = TIMEOUT_MS): Promise<unknown> {
    const controller = new AbortController();
    const cancel = this.env.schedule(() => controller.abort(), timeoutMs);
    try {
      return await request(this.env.fetch, path, controller.signal);
    } finally {
      cancel();
    }
  }
  /** One mutating board call with the login's CSRF token. It never retries:
   * the caller shows the error and the person decides. */
  async post(path: string, body: unknown, timeoutMs: number = TIMEOUT_MS): Promise<unknown> {
    const controller = new AbortController();
    const cancel = this.env.schedule(() => controller.abort(), timeoutMs);
    try {
      return await request(this.env.fetch, path, controller.signal, body, this.state.login?.csrf);
    } finally {
      cancel();
    }
  }
  private cancelTransport() {
    this.generation++;
    this.abort?.abort();
    this.abort = null;
    this.retry?.();
    this.retry = null;
    this.deadline?.();
    this.deadline = null;
    this.stable?.();
    this.stable = null;
    for (const w of this.watches.values()) {
      w.pending?.();
      w.pending = null;
    }
    const socket = this.socket;
    this.socket = null;
    socket?.close();
  }
  private anonymous(error: string | null) {
    this.cancelTransport();
    for (const w of this.watches.values()) w.idle?.();
    this.watches.clear();
    this.update({ ...emptyState(), auth: 'anonymous', error });
  }
  private async connect(path: string, body?: unknown): Promise<void> {
    this.cancelTransport();
    const gen = this.generation;
    this.hello = false;
    this.update({
      ...stale(this.state),
      auth: 'checking',
      error: null,
      transport: this.attempt === 0 ? 'connecting' : 'reconnecting',
    });
    const controller = new AbortController();
    this.abort = controller;
    const cancel = this.env.schedule(() => controller.abort(), TIMEOUT_MS);
    try {
      const login = authView(await request(this.env.fetch, path, controller.signal, body));
      if (gen !== this.generation || !this.running) return;
      this.update({ ...this.state, login, auth: 'authenticated' });
      this.open(gen);
    } catch (error) {
      if (gen !== this.generation || !this.running) return;
      const message = error instanceof Error ? error.message : 'Connection failed';
      if (error instanceof ApiError && error.status === 401) {
        this.anonymous(path === '/api/auth/session' ? null : message);
      } else if (body !== undefined) {
        // A password or a one-time token is never retained or retried.
        this.anonymous(message);
      } else {
        this.reconnect(message);
      }
    } finally {
      cancel();
    }
  }
  private open(gen: number) {
    const socket = this.env.socket();
    this.socket = socket;
    const current = () => this.running && gen === this.generation && this.socket === socket;
    this.deadline = this.env.schedule(() => {
      if (current()) this.reconnect('The board snapshot timed out');
    }, TIMEOUT_MS);
    socket.onopen = () => {
      if (current()) this.update({ ...this.state, transport: 'connected' });
    };
    socket.onerror = () => {
      if (current()) this.reconnect('The deck connection failed');
    };
    socket.onclose = ({ code }) => {
      if (!current()) return;
      if (code === 4401) this.anonymous('Your login ended. Log in again.');
      else this.reconnect('The deck connection ended');
    };
    socket.onmessage = ({ data }) => {
      if (!current()) return;
      try {
        const msg = boardMessage(data);
        if (msg.type === 'hello') {
          if (this.hello) throw new Error('Duplicate protocol hello');
          this.hello = true;
        } else if (!this.hello) throw new Error('The deck did not send a protocol hello');
        if (msg.type === 'snapshot' && this.state.freshness === 'live') return;
        this.update(reduceMessage(this.state, msg));
        if (msg.type === 'snapshot') {
          this.deadline?.();
          this.deadline = null;
          // A short-lived connection must not reset the backoff indefinitely.
          this.stable?.();
          this.stable = this.env.schedule(() => {
            if (current()) this.attempt = 0;
          }, 10_000);
          for (const w of this.watches.values()) if (w.users > 0) this.sendSubscription(w);
        }
        if (msg.type === 'timeline_start') {
          const w = this.watches.get(sessionKey(msg));
          w?.pending?.();
          if (w) w.pending = null;
        }
      } catch {
        this.reconnect('The deck sent an invalid board message');
      }
    };
  }
  private reconnect(error: string) {
    this.cancelTransport();
    this.update({ ...stale(this.state), transport: 'reconnecting', error });
    const cap = Math.min(30_000, 500 * 2 ** Math.min(this.attempt++, 6));
    this.retry = this.env.schedule(
      () => {
        this.retry = null;
        if (this.running) void this.connect('/api/auth/session');
      },
      Math.floor(cap * (0.5 + this.env.random() * 0.5)),
    );
  }
  private send(msg: BoardClientMessage) {
    try {
      this.socket?.send(JSON.stringify(msg));
    } catch {
      this.reconnect('The timeline request could not be sent');
    }
  }
  /** The presence heartbeat of an open session (R11.5, DL-166); a closed socket drops it. */
  presence = (key: SessionKey): void => {
    if (this.state.transport === 'connected') this.send({ type: 'presence', ...key });
  };
  watch(key: SessionKey): Cancel {
    const id = sessionKey(key);
    let w = this.watches.get(id);
    if (
      (!w || w.users === 0) &&
      [...this.watches.values()].filter((v) => v.users > 0).length >= 32
    ) {
      throw new Error('At most 32 timelines can be open');
    }
    if (!w) {
      w = { key, users: 0, idle: null, pending: null };
      this.watches.set(id, w);
      this.update({ ...this.state, timelines: { ...this.state.timelines, [id]: emptyTimeline() } });
    }
    w.idle?.();
    w.idle = null;
    if (w.users++ === 0 && this.state.freshness === 'live') this.sendSubscription(w);
    let active = true;
    return () => {
      if (!active) return;
      active = false;
      if (this.watches.get(id) !== w || --w.users > 0) return;
      w.pending?.();
      w.pending = null;
      if (this.state.transport === 'connected') this.send({ type: 'unsubscribe', ...key });
      w.idle = this.env.schedule(() => {
        if (this.watches.get(id) !== w || w.users > 0) return;
        this.watches.delete(id);
        const timelines = { ...this.state.timelines };
        delete timelines[id];
        this.update({ ...this.state, timelines });
      }, IDLE_MS);
    };
  }
  private sendSubscription(w: Watch) {
    const id = sessionKey(w.key);
    const t = this.state.timelines[id] ?? emptyTimeline();
    this.update({
      ...this.state,
      timelines: {
        ...this.state.timelines,
        [id]: {
          ...t,
          ready: false,
          freshness: 'stale',
          error: null,
        },
      },
    });
    w.pending?.();
    w.pending = this.env.schedule(() => {
      const current = this.state.timelines[id];
      if (!current || current.ready || w.users === 0) return;
      this.update({
        ...this.state,
        timelines: {
          ...this.state.timelines,
          [id]: {
            ...current,
            error: 'The timeline subscription timed out',
            freshness: 'stale',
          },
        },
      });
    }, TIMEOUT_MS);
    this.send({
      type: 'subscribe',
      ...w.key,
      incarnation: t.incarnation,
      after: t.incarnation === null ? null : t.cursor,
    });
  }
}
