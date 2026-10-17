// R6.1 (DL-169): the read cursor. While the board shows the end of a session's
// timeline, it tells the deck the highest `session_seq` it holds, one second after the
// last change. The deck keeps the cursor for this login and counts the unread events
// after it.
import { useEffect, useRef } from 'react';
import type { EventEnvelope, SessionKey } from '../gen';

/** The wait after the last change of the timeline before the cursor moves. */
export const MARK_DEBOUNCE_MS = 1000;

/** The highest `session_seq` of `events`, or 0. */
export function topSeq(events: readonly EventEnvelope[]): number {
  let top = 0;
  for (const e of events) if (e.session_seq > top) top = e.session_seq;
  return top;
}

type Mark = {
  readonly key: SessionKey | null;
  /** The timeline of the open session is loaded and live. */
  readonly ready: boolean;
  readonly events: readonly EventEnvelope[];
  /** The card's unread count; no mark is sent while it is 0. */
  readonly unread: number;
  /** The chat is on screen. */
  readonly visible: boolean;
  readonly send: ((key: SessionKey, seq: number) => Promise<void>) | undefined;
};

/** Sends the mark; a failed call is tried again at the next change. */
export function useMarkRead({ key, ready, events, unread, visible, send }: Mark): void {
  const sent = useRef(new Map<string, number>());
  const stationId = key?.station_id;
  const sessionId = key?.session_id;
  const seq = topSeq(events);
  useEffect(() => {
    if (send === undefined || stationId === undefined || sessionId === undefined) return undefined;
    if (!ready || !visible || unread <= 0 || seq === 0) return undefined;
    const ref = `${stationId}/${sessionId}`;
    if ((sent.current.get(ref) ?? 0) >= seq) return undefined;
    const timer = setTimeout(() => {
      if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return;
      sent.current.set(ref, seq);
      send({ station_id: stationId, session_id: sessionId }, seq).catch(() => {
        sent.current.delete(ref);
      });
    }, MARK_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [send, stationId, sessionId, ready, visible, unread, seq]);
}
