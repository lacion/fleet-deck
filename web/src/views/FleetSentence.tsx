// The one summary sentence of the live sessions (DESIGN-SYSTEM §4.3, §4.4): "6 live sessions on 4
// stations. 2 need you." The Fleet and the Watch headers say it in the same words and colors
// (home.css: `.fd-home-needs` in --act, calm in --ok, `.fd-home-conf` in --hazard).
import { liveSessionsText } from '../board/fleet';
import { countText } from '../ui/format';

/** The fleet total that the sentence says as "9 at work" (`atWork` of `board/agentCounts.ts`). */
export type AtWork = {
  readonly sessions: number;
  readonly agents: number;
  readonly partial: boolean;
};

/** "9 at work": the sessions that work plus their running agents; "9+ at work" for a lower bound. */
export function atWorkText(w: AtWork): string {
  return `${countText(w.sessions + w.agents)}${w.partial ? '+' : ''} at work`;
}

/** The split behind "9 at work" (§4.7): what it counts and what it leaves out. */
export function atWorkTitle(w: AtWork): string {
  const sessions = w.sessions === 1 ? '1 session' : `${countText(w.sessions)} sessions`;
  const what =
    w.agents === 0
      ? `${sessions} run now.`
      : `${sessions} and ${w.agents === 1 ? '1' : countText(w.agents)} of their agents run now.`;
  const note = w.partial ? ' Some workflow agents are not counted.' : '';
  return `${what} Sessions on unreachable stations are not counted.${note}`;
}

/** The header words when nothing needs you: "All clear" only without a conflict (§4.4). */
export function clearText(conflicts: number): string {
  return conflicts > 0 ? 'Nothing needs you' : 'All clear';
}

/**
 * The sentence, as inline parts for the summary line of a header: the live sessions, the stations
 * that run them (none: no station part), who needs you, then the conflicts. The sentence has the
 * whole width of the header under the title, so it drops no part (§4.3). `atWork` adds "9 at work"
 * after the stations part (the sessions that work plus their running agents); it shows only above 0.
 */
export function FleetSentence({
  live,
  stations,
  needsYou,
  conflicts = 0,
  atWork,
}: {
  readonly live: number;
  /** The stations that run a live session: 0 leaves out "on N stations". */
  readonly stations: number;
  readonly needsYou: number;
  readonly conflicts?: number;
  readonly atWork?: AtWork | undefined;
}) {
  return (
    <>
      <span>{liveSessionsText(live)}</span>
      {stations === 0 ? null : (
        <span className="fd-home-on">
          {' '}
          on <span>{stations === 1 ? '1 station' : `${stations} stations`}</span>
        </span>
      )}
      .{' '}
      {atWork === undefined || atWork.sessions + atWork.agents === 0 ? null : (
        <>
          <span className="fd-home-work" title={atWorkTitle(atWork)}>
            {atWorkText(atWork)}
          </span>
          .{' '}
        </>
      )}
      <span className="fd-home-state">
        {needsYou === 0 ? (
          <span className="fd-home-needs" data-clear="true">
            {clearText(conflicts)}
          </span>
        ) : (
          <span className="fd-home-needs">
            {needsYou === 1 ? '1 needs you' : `${needsYou} need you`}
          </span>
        )}
        .
      </span>
      {conflicts > 0 ? (
        <>
          {' '}
          <span className="fd-home-conf">
            {conflicts === 1 ? '1 conflict' : `${conflicts} conflicts`}
          </span>
          .
        </>
      ) : null}
    </>
  );
}
