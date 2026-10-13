// DL-141: the card sparkline. The deck sends no history, so the board counts
// the `session_seq` growth of each session while it is open, in one-minute buckets.
import type { DeckSession } from '../gen';
import { sessionRef } from '../ui/localSet';

/** How many one-minute buckets a session keeps. */
export const BUCKETS = 20;
const MINUTE = 60_000;

type Track = { seq: number; counts: Map<number, number> };

const tracks = new Map<string, Track>();

/** Adds the `session_seq` growth since the last call to the current minute of each session. */
export function record(sessions: readonly DeckSession[], now: number): void {
  const minute = Math.floor(now / MINUTE);
  for (const s of sessions) {
    const ref = sessionRef(s.station_id, s.summary.session_id);
    const seq = s.summary.session_seq;
    const track = tracks.get(ref);
    if (track === undefined) {
      tracks.set(ref, { seq, counts: new Map() });
      continue;
    }
    const grown = Math.max(0, seq - track.seq);
    track.seq = seq;
    if (grown > 0) track.counts.set(minute, (track.counts.get(minute) ?? 0) + grown);
    for (const old of track.counts.keys()) if (old <= minute - BUCKETS) track.counts.delete(old);
  }
}

/** The 20 counts of one session, oldest first; the last one is the minute of `now`. */
export function buckets(ref: string, now: number): number[] {
  const counts = tracks.get(ref)?.counts;
  const minute = Math.floor(now / MINUTE);
  return Array.from({ length: BUCKETS }, (_, i) => counts?.get(minute - (BUCKETS - 1) + i) ?? 0);
}
