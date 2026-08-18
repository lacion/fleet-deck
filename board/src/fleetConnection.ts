// One generation of the board's /state + /ws connection lifecycle.
//
// React may tear an effect down and immediately replay it (StrictMode), and a
// token change does the same in production. Every async continuation therefore
// owns an immutable generation object. A callback may touch the board only when
// that object is still the owner's current generation; WebSocket callbacks must
// additionally own the exact socket that produced them.

export type FleetConnectionStatus = 'live' | 'reconnecting' | 'offline';

export interface FleetConnectionOwner {
  current: object | null;
}

export interface FleetSocket {
  onopen: (() => void) | null;
  onmessage: ((event: { data: unknown }) => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
  close: () => void;
}

export interface FleetConnectionEnvironment {
  fetchState: (timeoutMs: number, signal: AbortSignal) => Promise<unknown>;
  openSocket: (url: string) => FleetSocket;
  socketUrl: () => string;
  schedule: (callback: () => void, delayMs: number) => unknown;
  cancel: (handle: unknown) => void;
  random: () => number;
}

export interface FleetConnectionCallbacks {
  onSnapshot: (snapshot: unknown) => void;
  onLan: (lan: unknown) => void;
  onStatus: (status: FleetConnectionStatus) => void;
}

interface ConnectionGeneration {
  ws: FleetSocket | null;
  retryTimer: unknown | null;
  pollTimer: unknown | null;
  pollController: AbortController | null;
  failures: number;
  healthFailures: number;
  closed: boolean;
  socketOpen: boolean;
  polling: boolean;
}

interface SnapshotFrame {
  type?: unknown;
  lan?: unknown;
}

function asSnapshotFrame(value: unknown): SnapshotFrame | null {
  return value !== null && typeof value === 'object' ? (value as SnapshotFrame) : null;
}

/** Start one independently-owned connection generation and return its teardown. */
export function startFleetConnection(
  owner: FleetConnectionOwner,
  callbacks: FleetConnectionCallbacks,
  env: FleetConnectionEnvironment,
): () => void {
  const st: ConnectionGeneration = {
    ws: null,
    retryTimer: null,
    pollTimer: null,
    pollController: null,
    failures: 0,
    healthFailures: 0,
    closed: false,
    socketOpen: false,
    polling: false,
  };
  owner.current = st;

  const ownsGeneration = () => owner.current === st && !st.closed;
  const ownsSocket = (ws: FleetSocket) => ownsGeneration() && st.ws === ws;

  const apply = (data: unknown) => {
    if (ownsGeneration()) callbacks.onSnapshot(data);
  };

  const queuePoll = (delay: number) => {
    if (!ownsGeneration()) return;
    if (st.pollTimer !== null) env.cancel(st.pollTimer);
    const timer = env.schedule(() => {
      if (!ownsGeneration() || st.pollTimer !== timer) return;
      st.pollTimer = null;
      pollOnce();
    }, delay);
    st.pollTimer = timer;
  };

  const stopPolling = () => {
    if (st.pollTimer !== null) {
      env.cancel(st.pollTimer);
      st.pollTimer = null;
    }
    st.pollController?.abort();
    st.pollController = null;
    st.polling = false;
  };

  // M-F3 — a /state poll started while the socket was down can still be in
  // flight when the WS opens; the WS snapshot is authoritative. The sole
  // exception is `lan`, which the daemon intentionally sends only via /state.
  const pollOnce = () => {
    if (!ownsGeneration() || st.polling) return;
    st.polling = true;
    const controller = new AbortController();
    st.pollController = controller;
    env
      .fetchState(st.socketOpen ? 5000 : 15_000, controller.signal)
      .then((raw) => {
        if (!ownsGeneration() || st.pollController !== controller || controller.signal.aborted)
          return;
        const data = asSnapshotFrame(raw);
        if (!data) {
          if (st.socketOpen) {
            st.healthFailures += 1;
            if (st.healthFailures >= 2) {
              callbacks.onStatus('reconnecting');
              try {
                st.ws?.close();
              } catch {
                /* reconnect watchdog owns recovery */
              }
            }
          }
          return;
        }
        st.healthFailures = 0;
        if (st.socketOpen) {
          if (data.lan) callbacks.onLan(data.lan);
        } else {
          apply(raw);
        }
      })
      .catch(() => {
        /* daemon unreachable — WS retry loop owns recovery */
      })
      .finally(() => {
        if (!ownsGeneration() || st.pollController !== controller) return;
        st.pollController = null;
        st.polling = false;
        // Browsers answer protocol-level WebSocket pings invisibly, so keep a
        // low-rate /state probe while live and retire the socket after two
        // misses. While disconnected, poll quickly enough to keep the board
        // useful before the next WebSocket attempt succeeds.
        queuePoll(st.socketOpen ? 15_000 : 3_000);
      });
  };

  const scheduleRetry = () => {
    if (!ownsGeneration()) return;
    st.ws = null;
    st.socketOpen = false;
    st.failures += 1;
    callbacks.onStatus(st.failures > 3 ? 'offline' : 'reconnecting');
    queuePoll(0);
    if (st.retryTimer !== null) env.cancel(st.retryTimer);
    const base = Math.min(500 * 2 ** (st.failures - 1), 8000);
    const delay = base * (0.8 + env.random() * 0.4);
    const timer = env.schedule(() => {
      if (!ownsGeneration() || st.retryTimer !== timer) return;
      st.retryTimer = null;
      connect();
    }, delay);
    st.retryTimer = timer;
  };

  const connect = () => {
    if (!ownsGeneration()) return;
    let ws: FleetSocket;
    try {
      ws = env.openSocket(env.socketUrl());
    } catch {
      scheduleRetry();
      return;
    }
    st.ws = ws;
    ws.onopen = () => {
      if (!ownsSocket(ws)) return;
      st.socketOpen = true;
      // OPEN is transport state, not application readiness. The daemon sends
      // a snapshot immediately; only that valid frame earns the LIVE badge.
      callbacks.onStatus('reconnecting');
    };
    ws.onmessage = (event) => {
      if (!ownsSocket(ws)) return;
      try {
        const data = asSnapshotFrame(JSON.parse(String(event.data)));
        if (data?.type === 'snapshot') {
          st.failures = 0;
          st.healthFailures = 0;
          callbacks.onStatus('live');
          apply(data);
        }
      } catch {
        /* malformed frame — ignore */
      }
    };
    ws.onclose = () => {
      if (ownsSocket(ws)) scheduleRetry();
    };
    ws.onerror = () => {
      if (!ownsSocket(ws)) return;
      try {
        ws.close();
      } catch {
        /* already closing */
      }
    };
  };

  callbacks.onStatus('reconnecting');
  pollOnce();
  connect();

  return () => {
    if (st.closed) return;
    st.closed = true;
    if (owner.current === st) owner.current = null;
    if (st.retryTimer !== null) {
      env.cancel(st.retryTimer);
      st.retryTimer = null;
    }
    stopPolling();
    const ws = st.ws;
    st.ws = null;
    st.socketOpen = false;
    try {
      ws?.close();
    } catch {
      /* unmounting */
    }
  };
}
