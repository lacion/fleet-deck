// R13 P2c: the station checkboxes of a display ceiling (station ids; a name could be renamed). A
// field row of its own (§4.7, `Field layout="row"`): the name in the label column, the stations in
// the control column. A station name is wide type (§2.3), as in the worker grant of the Deck tab.
import { useId } from 'react';
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
  const id = useId();
  return (
    <div className="fd-fieldwrap" data-layout="row" role="group" aria-labelledby={id}>
      <span id={id} className="fd-fieldlabel">
        {label}
      </span>
      {stations.length === 0 ? (
        <p className="fd-note fd-disp-nochoice">No station is paired.</p>
      ) : (
        <span className="fd-disp-choices">
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
              />
              <span className="fd-wide">{s.name}</span>
            </label>
          ))}
        </span>
      )}
    </div>
  );
}
