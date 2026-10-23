// R13 P1: the ambient mode: dark, low motion. It shows the counts, the tightest usage window and
// the next needs-you item. The wall moves the whole content by a few pixels (`ambientOffset`).
import { relativeAge } from '../app/sessions';
import { rateMeters, resetsText, tightest } from '../board/usage';
import type { DeckSession, StationView } from '../gen';
import { ALL_CLEAR, ambientCounts, type Need, usageParts } from './model';

export function AmbientPane({
  sessions,
  stations,
  needs,
  now,
}: {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly needs: readonly Need[];
  readonly now: number;
}) {
  const counts = ambientCounts(sessions, now);
  const meter = tightest(rateMeters(sessions, stations));
  const next = needs[0];
  return (
    <section className="fd-wall-ambient-body" aria-label="Fleet">
      <dl className="fd-wall-counts">
        <div>
          <dt>Running</dt>
          <dd>{counts.running}</dd>
        </div>
        <div data-tone={counts.needs > 0 ? 'act' : undefined}>
          <dt>Needs you</dt>
          <dd>{counts.needs}</dd>
        </div>
        <div>
          <dt>Done today</dt>
          <dd>{counts.done}</dd>
        </div>
      </dl>
      {meter === null || meter.usedPercent === null ? null : (
        // Each part keeps its words on one line, and a separator ends the part before it: a
        // line never starts with "·".
        <p className="fd-wall-ambient-line">
          <span className="fd-wall-part">{usageParts(meter).label} ·</span>{' '}
          <span className="fd-wall-part">
            <span className="fd-wall-fuel">{usageParts(meter).percent}</span> ·
          </span>{' '}
          <span className="fd-wall-part">
            {meter.station}
            {meter.resetsAt === null ? '' : ','}
          </span>
          {meter.resetsAt === null ? null : (
            <>
              {' '}
              <span className="fd-wall-part">{resetsText(meter.resetsAt, now)}</span>
            </>
          )}
        </p>
      )}
      <p className="fd-wall-ambient-line">
        {next === undefined ? (
          // The calm words of the board and of the mission wall (§4.4), in their green.
          <span className="fd-wall-clear">{ALL_CLEAR}</span>
        ) : (
          <>
            <span className="fd-wall-part">
              Next needs you:{' '}
              <span className="fd-callsign">
                {next.card.summary.callsign ?? next.card.summary.session_id}
              </span>{' '}
              ·
            </span>{' '}
            {/* The request title may be long: it wraps, and a no-break space keeps the
                separator on the line of its last word. */}
            {next.request.title}
            {'\u00a0·'}{' '}
            <span className="fd-wall-part">{relativeAge(next.request.requested_at, now)}</span>
          </>
        )}
      </p>
    </section>
  );
}
