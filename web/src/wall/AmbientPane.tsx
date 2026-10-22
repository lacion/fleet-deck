// R13 P1: the ambient mode: dark, low motion. It shows the counts, the tightest usage window and
// the next needs-you item. The wall moves the whole content by a few pixels (`ambientOffset`).
import { relativeAge } from '../app/sessions';
import { rateMeters, resetsText, tightest } from '../board/usage';
import type { DeckSession, StationView } from '../gen';
import { ambientCounts, type Need } from './model';

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
        <p className="fd-wall-ambient-line">
          {meter.label} · {meter.scope}: {Math.round(meter.usedPercent)}% used
          {meter.resetsAt === null ? '' : `, ${resetsText(meter.resetsAt, now)}`}
        </p>
      )}
      <p className="fd-wall-ambient-line">
        {next === undefined
          ? 'Nothing needs you.'
          : `Next needs you: ${next.card.summary.callsign ?? next.card.summary.session_id} · ${next.request.title} · ${relativeAge(next.request.requested_at, now)}`}
      </p>
    </section>
  );
}
