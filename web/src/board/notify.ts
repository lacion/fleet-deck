// M3 6-10: browser notifications for new needs-you requests (DL-305).
//
// The board shows one notification for each open request that appears
// after the owner turned notifications on. Requests that were open at the
// start or before the turn-on make no notification, so a reload makes no
// storm. The setting stays in this browser only.
import type { DeckSession, SessionKey } from '../gen';

export const NOTIFY_KEY = 'fleetdeck.notify';

export type Waiting = {
  readonly id: string;
  readonly key: SessionKey;
  readonly title: string;
  readonly request: string;
};

/** Every open request of the fleet, keyed `station/session/request`. */
export function waitingRequests(sessions: readonly DeckSession[]): Waiting[] {
  return sessions.flatMap((s) =>
    s.summary.open_requests.map((r) => ({
      id: `${s.station_id}/${s.summary.session_id}/${r.request_id}`,
      key: { station_id: s.station_id, session_id: s.summary.session_id },
      title: s.summary.title ?? s.summary.session_id,
      request: r.title,
    })),
  );
}

/** The requests in `now` that `seen` does not hold. */
export function freshRequests(seen: ReadonlySet<string>, now: readonly Waiting[]): Waiting[] {
  return now.filter((w) => !seen.has(w.id));
}

export type Permission = 'default' | 'granted' | 'denied' | 'unsupported';

/** The browser's notification permission. */
export function permission(): Permission {
  if (typeof Notification === 'undefined') return 'unsupported';
  return Notification.permission;
}

export function notifyEnabled(): boolean {
  try {
    return permission() === 'granted' && localStorage.getItem(NOTIFY_KEY) === 'on';
  } catch {
    return false;
  }
}

export function storeNotify(on: boolean): void {
  try {
    localStorage.setItem(NOTIFY_KEY, on ? 'on' : 'off');
  } catch {
    // A private window can refuse storage; the setting then lasts this page only.
  }
}

/** Shows one notification; a click focuses the board and opens the session. */
export function show(w: Waiting, onOpen: (key: SessionKey) => void): void {
  if (permission() !== 'granted') return;
  try {
    const n = new Notification(`Needs you: ${w.title}`, { body: w.request, tag: w.id });
    n.onclick = () => {
      window.focus();
      onOpen(w.key);
      n.close();
    };
  } catch {
    // Some mobile browsers allow notifications only from a service worker.
  }
}
