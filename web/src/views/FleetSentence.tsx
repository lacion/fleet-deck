// The one summary sentence of the live sessions (DESIGN-SYSTEM §4.3, §4.4): "6 live sessions on 4
// stations. 2 need you." The Fleet and the Watch headers say it in the same words and colors
// (home.css: `.fd-home-needs` in --act, calm in --ok, `.fd-home-conf` in --hazard).
import { liveSessionsText } from '../board/fleet';

/** The header words when nothing needs you: "All clear" only without a conflict (§4.4). */
export function clearText(conflicts: number): string {
  return conflicts > 0 ? 'Nothing needs you' : 'All clear';
}

/**
 * The sentence, as inline parts for the summary line of a header: the live sessions, the stations
 * they are on (none: no station part), who needs you, then the conflicts. Each part is a span, so a
 * narrow header can drop one (`.fd-home-on`, the calm `.fd-home-state[data-calm]`).
 */
export function FleetSentence({
  live,
  stations,
  needsYou,
  conflicts = 0,
}: {
  readonly live: number;
  /** The stations of the sentence: 0 leaves out "on N stations". */
  readonly stations: number;
  readonly needsYou: number;
  readonly conflicts?: number;
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
      {/* "Nothing needs you." next to a conflict is the calm part: a narrow header drops it
          first (home.css), so that the conflict stays readable. */}
      <span
        className="fd-home-state"
        data-calm={needsYou === 0 && conflicts > 0 ? 'true' : undefined}
      >
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
