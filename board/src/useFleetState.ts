// WS /ws snapshot subscription with reconnect + /state polling fallback.
//
// Connection lifecycle:
//   - initial GET /state paints the board before the socket is even open;
//   - WS /ws pushes {type:'snapshot', ...} on connect and on mutations;
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
import type { Snapshot, SpawnCapability, Lan, LegacyUpgrade } from '../../contracts/index.ts';
import {
  startFleetConnection,
  type FleetConnectionOwner,
  type FleetConnectionStatus,
  type FleetSocket,
} from './fleetConnection.ts';

// The connection pill's three states.
type ConnStatus = FleetConnectionStatus;

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
  // Keep the current and previous session arrays in one atomic state update.
  // A separate setState from inside setSnap's updater can be replayed by React
  // and make the spawn-failure watcher compare the wrong two frames.
  const [{ snap, prevSessions }, setFleet] = useState({
    snap: EMPTY,
    prevSessions: EMPTY.sessions,
  });
  const [status, setStatus] = useState<ConnStatus>('reconnecting'); // live | reconnecting | offline
  // Each effect invocation installs a NEW immutable generation object here.
  // Cleanup never mutates state later reused by a token change/StrictMode replay.
  const owner = useRef<FleetConnectionOwner['current']>(null);
  const { token, unauthorized } = useAuth();
  useEffect(() => {
    if (unauthorized) return undefined; // gated — App owns the screen now

    // H-S1 — the daemon deliberately keeps the token-bearing `lan` block OUT of
    // the WS broadcast (it rides only the token-gated GET /state). So a WS frame
    // carries no `lan`; preserve the last one we saw rather than clobbering the
    // share panel/LAN dot to null on every frame.
    const apply = (data: BoardSnapshot) => {
      setFleet((prev) => ({
        prevSessions: prev.snap.sessions,
        snap: { ...EMPTY, ...data, lan: data.lan ?? prev.snap.lan },
      }));
    };

    return startFleetConnection(
      owner,
      {
        // The daemon remains the runtime authority for the wire shape. The
        // lifecycle helper owns ordering/cancellation only; this hook owns the
        // imported Snapshot contract at the UI boundary.
        onSnapshot: (raw) => apply(raw as BoardSnapshot),
        onLan: (lan) => setFleet((prev) => ({ ...prev, snap: { ...prev.snap, lan: lan as Lan } })),
        onStatus: setStatus,
      },
      {
        fetchState: (timeoutMs, signal) => fetchState(timeoutMs, signal),
        openSocket: (url) => new WebSocket(url) as unknown as FleetSocket,
        socketUrl: () => wsUrl('/ws'),
        schedule: (callback, delayMs) => setTimeout(callback, delayMs),
        cancel: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
        random: Math.random,
      },
    );
    // a saved token must reconnect the socket that was refused without it
  }, [token, unauthorized]);

  return { snap, status, prevSessions };
}
