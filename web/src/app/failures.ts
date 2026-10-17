import type { DeckSession, SessionKey } from '../gen';
import { sessionRef } from '../ui/localSet';
import { sessionTitle } from './sessions';

/** A session that ended before it started stays in the banner this long (R7.1). */
export const START_FAILURE_WINDOW_MS = 10 * 60_000;

/** The station ends a start that never got a process with the error code `start_failed`. */
export const isStartFailure = (card: DeckSession): boolean =>
  card.status === 'ended' && card.summary.last_error?.code === 'start_failed';

/** The first line of an error text; the station already scrubbed the text. */
export const firstLine = (text: string): string => text.split('\n')[0]?.trim() ?? '';

export type StartFailure = {
  readonly ref: string;
  readonly key: SessionKey;
  readonly title: string;
  readonly reason: string;
};

/**
 * The sessions that did not start, one entry for each session. Old failures (a page that opens
 * after the failure) and failures that the owner dismissed are not listed.
 */
export function startFailures(
  sessions: readonly DeckSession[],
  now: number,
  dismissed: ReadonlySet<string>,
): StartFailure[] {
  const found: StartFailure[] = [];
  for (const card of sessions) {
    if (!isStartFailure(card) || now - card.summary.updated_at > START_FAILURE_WINDOW_MS) continue;
    const ref = sessionRef(card.station_id, card.summary.session_id);
    if (dismissed.has(ref)) continue;
    found.push({
      ref,
      key: { station_id: card.station_id, session_id: card.summary.session_id },
      title: sessionTitle(card),
      reason: firstLine(card.summary.last_error?.message ?? ''),
    });
  }
  return found;
}

/** The banner text of a start failure. */
export const failureText = (f: StartFailure): string =>
  `Session ${f.title} did not start${f.reason === '' ? '.' : `: ${f.reason}`}`;
