// R13 P2c: the station checkboxes of a display ceiling (station ids; a name could be renamed).
import type { StationView } from '../gen';

export function StationChoices({
  stations,
  value,
  onChange,
  label,
}: {
  readonly stations: readonly StationView[];
  readonly value: readonly string[];
  readonly onChange: (next: readonly string[]) => void;
  readonly label: string;
}) {
  return (
    <fieldset className="fd-disp-stations">
      <legend className="fd-fieldlabel">{label}</legend>
      {stations.length === 0 ? <p className="fd-note">No station is paired.</p> : null}
      {stations.map((s) => (
        <label key={s.station_id} className="fd-disp-check">
          <input
            type="checkbox"
            checked={value.includes(s.station_id)}
            onChange={(e) =>
              onChange(
                e.target.checked
                  ? [...value, s.station_id]
                  : value.filter((id) => id !== s.station_id),
              )
            }
          />{' '}
          {s.name}
        </label>
      ))}
    </fieldset>
  );
}
