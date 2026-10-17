// FG-6 (design §9.3): the placement fields of the spawn form under "Auto (best station)": the
// labels that the station needs, three preferences, and "Spread over stations" for a batch.
import type { Prefer, ProjectHome, ProviderKind, StationView } from '../gen';
import { AUTO } from './spawn';

/**
 * The Station select; for a project start (`homes` is not `null`) the Home select: the ready homes
 * as "<station> · <path>". `auto` adds "Auto (best station)".
 */
export function StationChoice({
  homes,
  targets,
  value,
  auto,
  onChange,
}: {
  readonly homes: readonly (ProjectHome & { readonly path: string })[] | null;
  readonly targets: readonly StationView[];
  readonly value: string;
  readonly auto: boolean;
  readonly onChange: (id: string) => void;
}) {
  const name = (id: string) => targets.find((t) => t.station_id === id)?.name ?? id;
  return (
    <label>
      {homes === null ? 'Station' : 'Home'}
      <select value={value} onChange={(event) => onChange(event.target.value)}>
        {homes === null
          ? targets.map((s) => (
              <option key={s.station_id} value={s.station_id}>
                {s.name}
              </option>
            ))
          : homes.map((h) => (
              <option key={h.station_id} value={h.station_id}>
                {`${name(h.station_id)} · ${h.path}`}
              </option>
            ))}
        {auto ? <option value={AUTO}>Auto (best station)</option> : null}
      </select>
    </label>
  );
}

/** The provider family under "Auto (best station)": the deck picks the instance on its station. */
export function KindChoice({
  kinds,
  value,
  onChange,
}: {
  readonly kinds: readonly ProviderKind[];
  readonly value: ProviderKind;
  readonly onChange: (kind: ProviderKind) => void;
}) {
  return (
    <label>
      Provider
      <select value={value} onChange={(event) => onChange(event.target.value as ProviderKind)}>
        {kinds.map((k) => (
          <option key={k} value={k}>
            {k.charAt(0).toUpperCase() + k.slice(1)}
          </option>
        ))}
      </select>
    </label>
  );
}

type Props = {
  readonly needs: string;
  readonly onNeeds: (text: string) => void;
  readonly prefer: readonly Prefer[];
  readonly onPrefer: (next: Prefer[]) => void;
  /** A batch (or `Nx`) of two or more sessions offers the spread switch. */
  readonly batch: boolean;
  readonly spread: boolean;
  readonly onSpread: (on: boolean) => void;
};

const CHOICES: readonly { readonly value: Prefer; readonly label: string }[] = [
  { value: 'idle', label: 'Idle station' },
  { value: 'not_on_battery', label: 'Not on battery' },
  { value: 'always_on', label: 'Always-on station' },
];

export function PlacementFields({
  needs,
  onNeeds,
  prefer,
  onPrefer,
  batch,
  spread,
  onSpread,
}: Props) {
  return (
    <div className="fd-spawn-placement">
      <label>
        Needs
        <input
          value={needs}
          spellCheck={false}
          autoCapitalize="off"
          autoComplete="off"
          onChange={(event) => onNeeds(event.target.value)}
        />
      </label>
      <div role="group" aria-label="Prefer">
        {CHOICES.map((c) => (
          <label key={c.value} className="fd-spawn-opt">
            <input
              type="checkbox"
              role="switch"
              className="fd-switch"
              checked={prefer.includes(c.value)}
              aria-checked={prefer.includes(c.value)}
              onChange={(event) =>
                onPrefer(
                  event.target.checked ? [...prefer, c.value] : prefer.filter((p) => p !== c.value),
                )
              }
            />
            {c.label}
          </label>
        ))}
      </div>
      {batch ? (
        <label className="fd-spawn-opt">
          <input
            type="checkbox"
            role="switch"
            className="fd-switch"
            checked={spread}
            aria-checked={spread}
            onChange={(event) => onSpread(event.target.checked)}
          />
          Spread over stations
        </label>
      ) : null}
    </div>
  );
}

/** The directory under "Auto (best station)" without a project: typed, the same on each station. */
export function TypedDirectory({
  value,
  onChange,
}: {
  readonly value: string;
  readonly onChange: (path: string) => void;
}) {
  return (
    <label>
      Directory
      <input
        value={value}
        aria-label="Directory path"
        placeholder="/work/repo"
        spellCheck={false}
        autoCapitalize="off"
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  );
}
