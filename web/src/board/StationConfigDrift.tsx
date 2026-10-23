// FG-11 (design §14.3): the config drift report on the Stations page. Each station is compared
// with the reference station: the two provider versions and the four hashes. Nothing syncs (O34).
import { type KeyboardEvent, useCallback, useEffect, useRef, useState } from 'react';
import type { ConfigDigest, DriftItem, StationView } from '../gen';
import { useIntegrations } from '../integrations/context';
import { message } from '../integrations/useLoad';
import { useDismiss } from '../ui/dismiss';

/** The label of each hash, in the order of the report. */
const ITEM_LABELS: readonly (readonly [DriftItem, string])[] = [
  ['claude_md', 'CLAUDE.md'],
  ['claude_agents', 'agents'],
  ['claude_skills', 'skills'],
  ['codex_agents_md', 'AGENTS.md'],
];

const hashOf = (c: ConfigDigest, name: DriftItem): string | undefined =>
  c.items.find((i) => i.name === name)?.hash;

/** The labels that differ between a station and the reference: `claude`, `codex`, then the items. */
export function driftLabels(reference: ConfigDigest, config: ConfigDigest): string[] {
  const out: string[] = [];
  if (reference.claude_version !== config.claude_version) out.push('claude');
  if (reference.codex_version !== config.codex_version) out.push('codex');
  for (const [name, label] of ITEM_LABELS) {
    if (hashOf(reference, name) !== hashOf(config, name)) out.push(label);
  }
  return out;
}

/**
 * The text of the line "Config: …" of one station, or `null` when the report has nothing to
 * compare (no reference, or a station without a config report): the line is then absent.
 */
export function configText(
  station: Pick<StationView, 'station_id' | 'info'>,
  reference: Pick<StationView, 'station_id' | 'info'> | undefined,
): string | null {
  if (reference !== undefined && reference.station_id === station.station_id) {
    return 'Config: Reference';
  }
  const config = station.info?.config;
  const base = reference?.info?.config;
  if (config === undefined || base === undefined) return null;
  const labels = driftLabels(base, config);
  return labels.length === 0 ? 'Config: Same' : `Config: Differs: ${labels.join(', ')}`;
}

/** The config reference of the deck (`GET /api/fleet-git/prefs`), and the call that sets it. */
export type DriftReference = {
  /** False until the deck answers; an older deck (no route) keeps it false and hides the report. */
  readonly offered: boolean;
  readonly station: string | null;
  readonly error: string | null;
  readonly set: (station: string) => Promise<void>;
};

export function useDriftReference(): DriftReference {
  const env = useIntegrations();
  const actions = env?.actions;
  const [offered, setOffered] = useState(false);
  const [station, setStation] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (actions === undefined) return undefined;
    let live = true;
    actions.fleetGitPrefs().then(
      (prefs) => {
        if (!live) return;
        setStation(prefs.drift_reference);
        setOffered(true);
      },
      () => live && setOffered(false),
    );
    return () => {
      live = false;
    };
  }, [actions]);
  const set = useCallback(
    async (next: string) => {
      if (actions === undefined) return;
      setError(null);
      try {
        const prefs = await actions.setDriftReference(next);
        setStation(prefs.drift_reference);
      } catch (e) {
        setError(message(e));
      }
    },
    [actions],
  );
  return { offered, station, error, set };
}

/** The line "Config: …" in a station card; nothing when there is nothing to compare. */
export function StationConfigDrift({
  station,
  reference,
}: {
  readonly station: StationView;
  readonly reference: StationView | undefined;
}) {
  const text = configText(station, reference);
  return text === null ? null : <span className="fd-station-config">{text}</span>;
}

/**
 * The config reference in the page header: a ghost menu toggle (a trailing chevron, base.css
 * `summary[data-variant="toggle"]`: it opens a menu, not a dialog) that says what it does
 * ("Compare configs" before a reference is picked, then "Configs vs Alpha") and lists the
 * stations. Without the admin scope it is a plain line, and nothing before a reference exists.
 * The popover behaves like `ui/Menu` (Escape and a press outside close it), whose summary takes
 * no variant.
 */
export function ConfigReferenceHeader({
  stations,
  reference,
  canSet,
  onSet,
}: {
  readonly stations: readonly StationView[];
  readonly reference: StationView | undefined;
  readonly canSet: boolean;
  readonly onSet: (station: string) => void;
}) {
  const ref = useRef<HTMLDetailsElement>(null);
  const [open, setOpen] = useState(false);
  const close = () => {
    if (ref.current) ref.current.open = false;
  };
  useDismiss(ref, open, close);
  const label = reference === undefined ? 'Compare configs' : `Configs vs ${reference.name}`;
  if (!canSet) {
    return reference === undefined ? null : <span className="fd-note fd-station-ref">{label}</span>;
  }
  const onKeyDown = (event: KeyboardEvent<HTMLDetailsElement>) => {
    if (event.key !== 'Escape' || !ref.current?.open) return;
    event.stopPropagation();
    close();
    ref.current.querySelector('summary')?.focus();
  };
  return (
    <details
      className="fd-menu"
      ref={ref}
      onKeyDown={onKeyDown}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="fd-ghost fd-sm" data-variant="toggle">
        {label}
      </summary>
      <div className="fd-menu-body">
        <span className="fd-menu-text">Pick a reference station for the config report.</span>
        {stations.map((s) => (
          <button
            key={s.station_id}
            type="button"
            onClick={() => {
              close();
              onSet(s.station_id);
            }}
          >
            {s.name}
          </button>
        ))}
      </div>
    </details>
  );
}
