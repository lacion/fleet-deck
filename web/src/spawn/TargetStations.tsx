import { stationName } from '../board/fleet';
import type { StationView } from '../gen';

type Props = {
  readonly stations: readonly StationView[];
  /** The station of the new session: always in the grant, so it is not a choice. */
  readonly ownId: string;
  readonly picked: readonly string[];
  readonly onChange: (next: readonly string[]) => void;
};

/** The choices: every paired station except the session's own, in the order of the list. */
export function targetChoices(stations: readonly StationView[], ownId: string): StationView[] {
  return stations.filter((s) => s.station_id !== ownId);
}

/** ADR-006 "Creation": the target stations of a coordinator grant (owner only; default none). */
export function TargetStations({ stations, ownId, picked, onChange }: Props) {
  const choices = targetChoices(stations, ownId);
  return (
    <fieldset className="fd-spawn-fleet">
      <legend>Target stations</legend>
      <label className="fd-spawn-opt">
        <input type="checkbox" checked disabled />
        This station (always)
      </label>
      {choices.map((s) => (
        <label key={s.station_id} className="fd-spawn-opt">
          <input
            type="checkbox"
            checked={picked.includes(s.station_id)}
            onChange={(event) =>
              onChange(
                event.target.checked
                  ? [...picked, s.station_id]
                  : picked.filter((id) => id !== s.station_id),
              )
            }
          />
          {stationName(s.station_id, stations)}
        </label>
      ))}
      <p className="fd-note">
        The coordinator can act on sessions of these stations. Each one needs a current link with
        coordinator v2.
      </p>
    </fieldset>
  );
}
