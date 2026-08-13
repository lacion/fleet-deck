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
//
// v1.7 LAN mode: the token rides the WS query string (`?t=` — browsers cannot
// set headers on a WS handshake) and the Authorization header on /state. A WS
// refused for auth looks like any other drop to the browser (no status code
// reaches JS), so the 401 that actually LATCHES the gate always comes from the
// /state poll — which is exactly why the poll fires on boot and on every drop.
// While the board is gated we stop connecting entirely: no retry storm behind
// a wall we know is up. Saving a token re-runs this effect and reconnects.
//
// The board's view of the snapshot is `BoardSnapshot` below — the daemon's
// `Snapshot` wire contract (contracts/state.ts) relaxed for version skew. This
// replaces the hand-maintained comment-contract that used to live on `EMPTY`:
// the shape is now imported, so daemon/board drift is a type error.

import { useEffect, useRef, useState } from 'react';
import { fetchState } from './api.ts';
import { useAuth, wsUrl } from './token.ts';
import type {
  Snapshot,
  SpawnCapability,
  Lan,
  LegacyUpgrade,
  SessionEntry,
} from '../../contracts/index.ts';

// The connection pill's three states.
type ConnStatus = 'live' | 'reconnecting' | 'offline';

// Fields the board ALWAYS has a safe default for (seeded by EMPTY), so consumers
// map them without a load guard.
type SnapshotDefaults =
  | 'up_ms'
  | 'sessions'
  | 'repos'
  | 'ticker'
  | 'conflicts'
  | 'mail_pending'
  | 'mail_meta'
  | 'questions'
  | 'spawn_orphans';

// Fields that arrive only with a /state or /ws payload: undefined until the
// first frame, and `plans` stays undefined against a daemon too old to send it
// (< v1.3) — which is exactly how the board decides to hide the plan library.
type SnapshotLoaded =
  | 'schema_version'
  | 'uptime_ms'
  | 'version'
  | 'repo_catalog'
  | 'settings'
  | 'home_dir'
  | 'plans';

// The board projection of the daemon `Snapshot`. Two honest relaxations over the
// wire contract, both for version-skew tolerance:
//   • `spawn` is `SpawnCapability | null` — null disables the spawn UI on a
//     pre-v1.2 daemon (and is the boot default before the first frame);
//   • `lan` is `Lan | null` — WS frames never carry it (H-S1) and pre-v1.7
//     daemons omit it, so the hook holds the last value it saw, or null.
// SnapshotDefaults stay required; SnapshotLoaded are optional (see above).
export type BoardSnapshot = Pick<Snapshot, SnapshotDefaults> &
  Partial<Pick<Snapshot, SnapshotLoaded>> & {
    spawn: SpawnCapability | null;
    lan: Lan | null;
    // `legacy_upgrade` rides the /ws + /state frames (Header reads it) but the
    // daemon puts it on StateResponse/WsSnapshot, NOT the base `Snapshot` — so,
    // like spawn/lan, the board carries it as its own null-defaulted field.
    legacy_upgrade: LegacyUpgrade | null;
  };

// The mutable connection state carried across renders in a ref. `timer`/`poll`
// are timer handles (browser `setTimeout`/`setInterval` return `number`).
interface ConnRef {
  ws: WebSocket | null;
  timer: ReturnType<typeof setTimeout> | null;
  poll: ReturnType<typeof setInterval> | null;
  failures: number;
  closed: boolean;
  socketOpen: boolean;
}

const EMPTY: BoardSnapshot = {
  up_ms: 0,
  sessions: [],
  repos: [],
  ticker: [],
  conflicts: [],
  mail_pending: {},
  mail_meta: {}, // {sid: {queued, oldest_at, route}} — route: watcher|pane|turn-boundary|offline-queued
  questions: [],
  spawn: null, // v1.2 capability: {available, reason?, active}
  spawn_orphans: [],
  // v1.7 LAN share — {enabled, urls}. Absent on older daemons, so `null` is
  // the honest default: the panel says "loopback-only", never invents a URL.
  lan: null,
  // No pre-upgrade sessions until a frame says otherwise (same null-default as
  // spawn/lan above; the field is not on the base Snapshot contract).
  legacy_upgrade: null,
  // v1.3 `plans` is deliberately ABSENT here: a daemon that doesn't send it
  // leaves snap.plans undefined and the board hides the library entirely.
};

export function useFleetState() {
  const [snap, setSnap] = useState<BoardSnapshot>(EMPTY);
  const [status, setStatus] = useState<ConnStatus>('reconnecting'); // live | reconnecting | offline
  const ref = useRef<ConnRef>({
    ws: null,
    timer: null,
    poll: null,
    failures: 0,
    closed: false,
    socketOpen: false,
  });
  const { token, unauthorized } = useAuth();
  // The previous frame's sessions, kept one frame behind `snap` (2.3): the
  // spawn-failure banner needs "a card TRANSITIONED to offline", and a card
  // that was already offline when the board loaded must not count as one.
  const [prevSessions, setPrevSessions] = useState<SessionEntry[]>(EMPTY.sessions);

  useEffect(() => {
    if (unauthorized) return undefined; // gated — App owns the screen now
    const st = ref.current;
    st.closed = false;
    st.failures = 0;
    st.socketOpen = false;

    // H-S1 — the daemon deliberately keeps the token-bearing `lan` block OUT of
    // the WS broadcast (it rides only the token-gated GET /state). So a WS frame
    // carries no `lan`; preserve the last one we saw rather than clobbering the
    // share panel/LAN dot to null on every frame.
    const apply = (data: BoardSnapshot) => {
      setSnap((prev) => {
        setPrevSessions(prev.sessions);
        return { ...EMPTY, ...data, lan: data.lan ?? prev.lan };
      });
    };

    // M-F3 — a /state poll started while the socket was down can still be in
    // flight when the WS opens; the WS pushes the authoritative snapshot on
    // connect and on every mutation, so once the socket is open the poll result
    // is by definition NOT newer and must not overwrite the board — a late poll
    // landing after a fresh WS frame would regress it to an older snapshot.
    // EXCEPTION: `lan` lives ONLY on /state (H-S1 above), so even after the
    // socket is open we still fold that one field in from a poll (it can't
    // regress anything the WS owns).
    const pollOnce = () => {
      fetchState()
        // api.js is still JS (it converts in Phase 8), so fetchState() is typed
        // `any`; assert the board's snapshot shape at this boundary. The board
        // trusts the daemon's wire shape by design — runtime validation is the
        // daemon's job (contracts/), never the browser's. See ts-migration-bugs.md;
        // this cast drops once api.js is typed to return the wire contract.
        .then((raw) => {
          const data = raw as BoardSnapshot | null;
          if (!data || st.closed) return;
          if (st.socketOpen) {
            if (data.lan) setSnap((prev) => ({ ...prev, lan: data.lan }));
          } else {
            apply(data);
          }
        })
        .catch(() => {
          /* daemon unreachable — WS retry loop owns recovery */
        });
    };

    const startPolling = () => {
      if (st.poll) return;
      st.poll = setInterval(pollOnce, 3000);
    };
    const stopPolling = () => {
      if (st.poll) {
        clearInterval(st.poll);
        st.poll = null;
      }
    };

    const connect = () => {
      if (st.closed) return;
      let ws: WebSocket;
      try {
        ws = new WebSocket(wsUrl('/ws'));
      } catch {
        scheduleRetry();
        return;
      }
      st.ws = ws;
      ws.onopen = () => {
        st.failures = 0;
        st.socketOpen = true;
        setStatus('live');
        stopPolling();
      };
      ws.onmessage = (e) => {
        try {
          // The frame is JSON from the daemon; assert the board snapshot shape
          // (plus the WS-only `type` discriminator) rather than runtime-validate
          // — the same daemon-trust boundary as the /state poll above.
          const data = JSON.parse(e.data as string) as (BoardSnapshot & { type?: string }) | null;
          if (data?.type === 'snapshot') apply(data);
        } catch {
          /* malformed frame — ignore */
        }
      };
      ws.onclose = () => {
        if (st.ws === ws) scheduleRetry();
      };
      ws.onerror = () => {
        try {
          ws.close();
        } catch {
          /* already closing */
        }
      };
    };

    const scheduleRetry = () => {
      if (st.closed) return;
      st.ws = null;
      st.socketOpen = false; // poll results may be applied again while we're down
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
      clearTimeout(st.timer ?? undefined);
      stopPolling();
      try {
        st.ws?.close();
      } catch {
        /* unmounting */
      }
    };
    // a saved token must reconnect the socket that was refused without it
  }, [token, unauthorized]);

  return { snap, status, prevSessions };
}
