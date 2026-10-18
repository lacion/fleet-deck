// R13 P6 §12: the desktop limits of a coordinator grant and its recent desktop operations.
import { useState } from 'react';
import { stationName } from '../board/fleet';
import type { GrantView, StationView } from '../gen';
import type { ElevationAsk } from '../settings/elevation';
import { LimitInput } from '../spawn/FleetControlFields';
import {
  captureCapProblem,
  desktopCapProblem,
  type FleetControl,
  imageBudgetProblem,
  NO_FLEET,
} from '../spawn/fleetControl';
import { type DesktopOpRow, type FleetActions, type FleetResult, mib } from './fleet';

const MIB = 1_048_576;
const UNRESOLVED: readonly string[] = ['prepared', 'attempted', 'unknown'];

/** The text inputs of the limits form. */
export type LimitValues = {
  readonly desktopCap: string;
  readonly captureCap: string;
  readonly imageMib: string;
};

/** The values of a grant, as the form shows them. */
export function limitValues(g: GrantView): LimitValues {
  return {
    desktopCap: String(g.desktop_cap ?? 0),
    captureCap: String(g.capture_cap ?? 0),
    imageMib: mib(g.image_byte_cap),
  };
}

/** The form values as a fleet-control value, to reuse the problem texts of the spawn form. */
function asFleet(g: GrantView, v: LimitValues): FleetControl {
  return {
    ...NO_FLEET,
    desktop: true,
    capture: g.capture_cap !== undefined,
    ...v,
  };
}

/** The problems of the form values, in the order of the inputs; empty when all are valid. */
export function limitProblems(g: GrantView, v: LimitValues): readonly string[] {
  const f = asFleet(g, v);
  return [desktopCapProblem(f), captureCapProblem(f), imageBudgetProblem(f)].filter(
    (p): p is string => p !== null,
  );
}

type Caps = { desktop_cap?: number; capture_cap?: number; image_byte_cap?: number };

/** The `caps` body of the form values: one value for each limit that the grant has. */
export function limitCaps(g: GrantView, v: LimitValues): Caps {
  return {
    desktop_cap: Number(v.desktopCap.trim()),
    ...(g.capture_cap === undefined ? {} : { capture_cap: Number(v.captureCap.trim()) }),
    ...(g.image_byte_cap === undefined
      ? {}
      : { image_byte_cap: Math.round(Number(v.imageMib.trim()) * MIB) }),
  };
}

/** True when any value of `caps` is higher than the current limit of the grant. */
export function raises(g: GrantView, caps: Caps): boolean {
  return (
    (caps.desktop_cap ?? 0) > (g.desktop_cap ?? 0) ||
    (caps.capture_cap ?? 0) > (g.capture_cap ?? 0) ||
    (caps.image_byte_cap ?? 0) > (g.image_byte_cap ?? 0)
  );
}

/**
 * Saves the limits. An increase asks the elevation first (label `Desktop limits`); a cancelled
 * dialog sends nothing and answers `null`.
 */
export async function saveLimits(
  g: GrantView,
  v: LimitValues,
  actions: Pick<FleetActions, 'caps'>,
  ask: ElevationAsk,
): Promise<FleetResult | null> {
  const caps = limitCaps(g, v);
  if (!raises(g, caps)) return actions.caps(g.grant_id, caps);
  const code = await ask(['Desktop limits']);
  return code === null ? null : actions.caps(g.grant_id, caps, code);
}

/** The words of an operation row: tool, station, state, the code, and the unresolved mark. */
export function opLine(r: DesktopOpRow, stations: readonly StationView[]): string {
  const code = r.code === null ? '' : ` (${r.code})`;
  const open = UNRESOLVED.includes(r.state) ? ' — unresolved' : '';
  return `${r.tool} on ${stationName(r.target_station, stations)}: ${r.state}${code}${open}`;
}

type FormProps = {
  readonly grant: GrantView;
  readonly values: LimitValues;
  readonly busy: boolean;
  readonly onChange: (next: LimitValues) => void;
  readonly onSave: () => void;
  readonly onCancel: () => void;
};

/** The inline form of the limits, prefilled from the grant. */
export function LimitsForm({ grant, values, busy, onChange, onSave, onCancel }: FormProps) {
  const f = asFleet(grant, values);
  const problems = limitProblems(grant, values);
  return (
    <form
      className="fd-fleet-form"
      aria-label="Change limits"
      onSubmit={(event) => {
        event.preventDefault();
        if (problems.length === 0 && !busy) onSave();
      }}
    >
      <LimitInput
        label="Desktop action limit"
        value={values.desktopCap}
        max={5000}
        problem={desktopCapProblem(f)}
        onChange={(desktopCap) => onChange({ ...values, desktopCap })}
      />
      {grant.capture_cap === undefined ? null : (
        <LimitInput
          label="Screenshot limit"
          value={values.captureCap}
          max={5000}
          problem={captureCapProblem(f)}
          onChange={(captureCap) => onChange({ ...values, captureCap })}
        />
      )}
      {grant.image_byte_cap === undefined ? null : (
        <LimitInput
          label="Image read budget (MiB)"
          value={values.imageMib}
          max={1024}
          problem={imageBudgetProblem(f)}
          onChange={(imageMib) => onChange({ ...values, imageMib })}
        />
      )}
      <div className="fd-fleet-actions">
        <button type="submit" disabled={busy || problems.length > 0}>
          Save
        </button>
        <button type="button" disabled={busy} onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}

type Props = {
  readonly grant: GrantView;
  readonly stations: readonly StationView[];
  readonly actions: FleetActions;
  /** The elevation of the owner; `undefined` for a login that cannot change limits. */
  readonly ask: ElevationAsk | undefined;
  /** Called after a change, to load the grants again. */
  readonly onChanged: () => void;
};

/** The limits and the recent desktop operations of one grant that has `desktop_cap`. */
export function GrantDesktop({ grant, stations, actions, ask, onChanged }: Props) {
  const [editing, setEditing] = useState(false);
  const [values, setValues] = useState<LimitValues>(() => limitValues(grant));
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<FleetResult | null>(null);
  const [ops, setOps] = useState<readonly DesktopOpRow[] | null>(null);
  const [opsOpen, setOpsOpen] = useState(false);
  const save = () => {
    if (ask === undefined) return;
    setBusy(true);
    void saveLimits(grant, values, actions, ask)
      .then((r) => {
        if (r === null) return;
        setResult(r);
        if (r.ok) setEditing(false);
        onChanged();
      })
      .finally(() => setBusy(false));
  };
  const toggleOps = () => {
    const next = !opsOpen;
    setOpsOpen(next);
    if (next) void actions.desktopOps(grant.grant_id).then(setOps);
  };
  return (
    <div className="fd-grant-desktop">
      {grant.state === 'revoked' || ask === undefined || editing ? null : (
        <button
          type="button"
          onClick={() => {
            setValues(limitValues(grant));
            setResult(null);
            setEditing(true);
          }}
        >
          Change limits
        </button>
      )}
      <button type="button" aria-expanded={opsOpen} onClick={toggleOps}>
        Desktop operations
      </button>
      {editing ? (
        <LimitsForm
          grant={grant}
          values={values}
          busy={busy}
          onChange={setValues}
          onSave={save}
          onCancel={() => setEditing(false)}
        />
      ) : null}
      {result === null ? null : (
        <p className="fd-note" role="status">
          {result.text}
        </p>
      )}
      {opsOpen && ops !== null && ops.length > 0 ? (
        <ul className="fd-fleet-list" aria-label="Desktop operations">
          {ops.map((r) => (
            <li key={r.operation_id}>{opLine(r, stations)}</li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
