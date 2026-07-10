// WS /ws snapshot subscription with reconnect + /state polling fallback.
//
// Connection lifecycle:
//   - initial GET /state paints the board before the socket is even open;
//   - WS /ws pushes {type:'snapshot', ...} on every mutation + 5 s heartbeat;
//   - on drop: status → 'reconnecting', retry with exponential backoff
//     (500 ms → 8 s cap, ±20% jitter); after 3 straight failures the pill
//     reads OFFLINE (still retrying underneath, forever);
//   - while not live, GET /state is polled every 3 s so the board keeps
//     breathing off the same snapshot shape (WS and /state are identical
//     minus the `type` field).

import { useEffect, useRef, useState } from 'react';

const EMPTY = {
  up_ms: 0,
  sessions: [],
  repos: [],
  ticker: [],
  conflicts: [],
  mail_pending: {},
  questions: [],
  spawn: null, // v1.2 capability: {available, reason?, max, active}
  spawn_orphans: [],
  // v1.3 `plans` is deliberately ABSENT here: a daemon that doesn't send it
  // leaves snap.plans undefined and the board hides the library entirely.
};

export function useFleetState() {
  const [snap, setSnap] = useState(EMPTY);
  const [status, setStatus] = useState('reconnecting'); // live | reconnecting | offline
  const ref = useRef({ ws: null, timer: null, poll: null, failures: 0, closed: false });

  useEffect(() => {
    const st = ref.current;
    st.closed = false;

    const apply = (data) => setSnap({ ...EMPTY, ...data });

    const pollOnce = () => {
      fetch('/state')
        .then((r) => (r.ok ? r.json() : null))
        .then((data) => { if (data && !st.closed) apply(data); })
        .catch(() => { /* daemon unreachable — WS retry loop owns recovery */ });
    };

    const startPolling = () => {
      if (st.poll) return;
      st.poll = setInterval(pollOnce, 3000);
    };
    const stopPolling = () => {
      if (st.poll) { clearInterval(st.poll); st.poll = null; }
    };

    const connect = () => {
      if (st.closed) return;
      const proto = location.protocol === 'https:' ? 'wss' : 'ws';
      let ws;
      try {
        ws = new WebSocket(`${proto}://${location.host}/ws`);
      } catch {
        scheduleRetry();
        return;
      }
      st.ws = ws;
      ws.onopen = () => {
        st.failures = 0;
        setStatus('live');
        stopPolling();
      };
      ws.onmessage = (e) => {
        try {
          const data = JSON.parse(e.data);
          if (data && data.type === 'snapshot') apply(data);
        } catch { /* malformed frame — ignore */ }
      };
      ws.onclose = () => { if (st.ws === ws) scheduleRetry(); };
      ws.onerror = () => { try { ws.close(); } catch { /* already closing */ } };
    };

    const scheduleRetry = () => {
      if (st.closed) return;
      st.ws = null;
      st.failures += 1;
      setStatus(st.failures > 3 ? 'offline' : 'reconnecting');
      startPolling();
      const base = Math.min(500 * 2 ** (st.failures - 1), 8000);
      const delay = base * (0.8 + Math.random() * 0.4);
      st.timer = setTimeout(connect, delay);
    };

    pollOnce();
    connect();

    return () => {
      st.closed = true;
      clearTimeout(st.timer);
      stopPolling();
      try { st.ws?.close(); } catch { /* unmounting */ }
    };
  }, []);

  return { snap, status };
}
