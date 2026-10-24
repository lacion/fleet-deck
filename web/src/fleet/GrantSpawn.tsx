// MCP P4a §8: the session limit and the highest modes of a grant.
import { useState } from 'react';
import type { GrantView } from '../gen';
import type { ElevationAsk } from '../settings/elevation';
import { CLAUDE_CEILINGS, CODEX_CEILINGS, LimitInput } from '../spawn/FleetControlFields';
import type { FleetActions, FleetResult } from './fleet';
import { ceilingLabel } from './modes';

/** The select value of a family without a ceiling; its option text is "Not allowed". */
export const NOT_ALLOWED = '';

/** The inputs of the session rules form. */
export type SpawnValues = {
  readonly spawnCap: string;
  readonly claude: string;
  readonly codex: string;
};

/** The `caps` body of the session rules: only the fields that changed. */
export type SpawnBody = {
  spawn_cap?: number;
  max_modes?: { [key in string]: string };
};

const CLAUDE_ORDER: readonly string[] = CLAUDE_CEILINGS;
const POLICIES: readonly string[] = ['untrusted', 'on-request', 'never'];
const SANDBOXES: readonly string[] = ['read-only', 'workspace-write', 'danger-full-access'];

/** The title of a grant row. */
export function grantTitle(g: GrantView): string {
  return `${g.kind === 'worker' ? 'Worker' : 'Coordinator'} ${g.session_id}`;
}

/** The values of a grant, as the form shows them. */
export function spawnValues(g: GrantView): SpawnValues {
  return {
    spawnCap: String(g.spawn_cap),
    claude: g.max_modes?.['claude'] ?? NOT_ALLOWED,
    codex: g.max_modes?.['codex'] ?? NOT_ALLOWED,
  };
}

/** True when the grant has the class `spawn`: it has a session limit. */
export const hasLimit = (g: GrantView): boolean => g.classes.includes('spawn');

/** True when the grant has the class `spawn` or `manage`: it has mode ceilings. */
export const hasCeilings = (g: GrantView): boolean =>
  g.classes.includes('spawn') || g.classes.includes('manage');

const limitMax = (g: GrantView): number => (g.kind === 'worker' ? 10 : 50);

/** The problem of the session limit text, or null (only checked for a grant with the class `spawn`). */
export function sessionLimitProblem(g: GrantView, v: SpawnValues): string | null {
  if (!hasLimit(g)) return null;
  const max = limitMax(g);
  const t = v.spawnCap.trim();
  if (/^\d+$/.test(t) && Number(t) <= max) return null;
  return `The session limit is a whole number from 0 to ${max}.`;
}

/** True when `mode` is not higher than `ceiling` in the order of the deck; an unknown value is false. */
export function modeWithin(family: 'claude' | 'codex', mode: string, ceiling: string): boolean {
  if (family === 'claude') {
    const a = CLAUDE_ORDER.indexOf(mode);
    const b = CLAUDE_ORDER.indexOf(ceiling);
    return a >= 0 && b >= 0 && a <= b;
  }
  const [mp, ms, ...mr] = mode.split('/');
  const [cp, cs, ...cr] = ceiling.split('/');
  if (mp === undefined || ms === undefined || cp === undefined || cs === undefined) return false;
  if (mr.length > 0 || cr.length > 0) return false;
  const pa = POLICIES.indexOf(mp);
  const pb = POLICIES.indexOf(cp);
  const sa = SANDBOXES.indexOf(ms);
  const sb = SANDBOXES.indexOf(cs);
  return pa >= 0 && pb >= 0 && sa >= 0 && sb >= 0 && pa <= pb && sa <= sb;
}

/** The `caps` body of the form values: only the fields that changed; `{}` when nothing changed. */
export function spawnBody(g: GrantView, v: SpawnValues): SpawnBody {
  const was = spawnValues(g);
  const body: SpawnBody = {};
  if (hasLimit(g) && Number(v.spawnCap.trim()) !== g.spawn_cap) {
    body.spawn_cap = Number(v.spawnCap.trim());
  }
  if (hasCeilings(g) && (v.claude !== was.claude || v.codex !== was.codex)) {
    body.max_modes = {
      ...(v.claude === NOT_ALLOWED ? {} : { claude: v.claude }),
      ...(v.codex === NOT_ALLOWED ? {} : { codex: v.codex }),
    };
  }
  return body;
}

/** True when `body` raises the session limit, adds a family, or raises the mode of a family. */
export function spawnWidens(g: GrantView, body: SpawnBody): boolean {
  if (body.spawn_cap !== undefined && body.spawn_cap > g.spawn_cap) return true;
  if (body.max_modes === undefined) return false;
  const old = g.max_modes ?? {};
  for (const family of ['claude', 'codex'] as const) {
    const next = body.max_modes[family];
    if (next === undefined) continue;
    const before = old[family];
    if (before === undefined || !modeWithin(family, next, before)) return true;
  }
  return false;
}

/**
 * Saves the session rules. A widening asks the elevation first (label `Session limits`); a
 * cancelled dialog sends nothing and answers `null`. No change sends nothing.
 */
export async function saveSpawn(
  g: GrantView,
  v: SpawnValues,
  actions: Pick<FleetActions, 'caps'>,
  ask: ElevationAsk,
): Promise<FleetResult | null> {
  const body = spawnBody(g, v);
  if (body.spawn_cap === undefined && body.max_modes === undefined) {
    return { ok: true, text: 'Nothing changed.' };
  }
  if (!spawnWidens(g, body)) return actions.caps(g.grant_id, body);
  const code = await ask(['Session limits']);
  return code === null ? null : actions.caps(g.grant_id, body, code);
}

type FormProps = {
  readonly grant: GrantView;
  readonly values: SpawnValues;
  readonly busy: boolean;
  readonly onChange: (next: SpawnValues) => void;
  readonly onSave: () => void;
  readonly onCancel: () => void;
};

/** The inline form of the session rules, prefilled from the grant. */
export function SpawnForm({ grant, values, busy, onChange, onSave, onCancel }: FormProps) {
  const problem = sessionLimitProblem(grant, values);
  return (
    <form
      className="fd-fleet-form"
      aria-label="Change session rules"
      onSubmit={(event) => {
        event.preventDefault();
        if (problem === null && !busy) onSave();
      }}
    >
      {hasLimit(grant) ? (
        <LimitInput
          label="Session limit"
          value={values.spawnCap}
          max={limitMax(grant)}
          problem={problem}
          onChange={(spawnCap) => onChange({ ...values, spawnCap })}
        />
      ) : null}
      {hasCeilings(grant) ? (
        <>
          <label>
            Highest Claude mode
            <select
              value={values.claude}
              onChange={(event) => onChange({ ...values, claude: event.target.value })}
            >
              <option value={NOT_ALLOWED}>Not allowed</option>
              {CLAUDE_CEILINGS.map((mode) => (
                <option key={mode} value={mode}>
                  {ceilingLabel(mode)}
                </option>
              ))}
            </select>
          </label>
          <label>
            Highest Codex mode
            <select
              value={values.codex}
              onChange={(event) => onChange({ ...values, codex: event.target.value })}
            >
              <option value={NOT_ALLOWED}>Not allowed</option>
              {CODEX_CEILINGS.map((mode) => (
                <option key={mode} value={mode}>
                  {ceilingLabel(mode)}
                </option>
              ))}
            </select>
          </label>
          <p className="fd-note">
            Sessions that it starts, resumes or changes never get a higher mode. A higher value
            needs your elevation.
          </p>
        </>
      ) : null}
      <div className="fd-fleet-actions">
        <button type="submit" disabled={busy || problem !== null}>
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
  readonly actions: FleetActions;
  /** The elevation of the owner; `undefined` for a login that cannot change limits. */
  readonly ask: ElevationAsk | undefined;
  /** Called after a change, to load the grants again. */
  readonly onChanged: () => void;
};

/** The session limit and the highest modes of one grant that has the class `spawn` or `manage`. */
export function GrantSpawn({ grant, actions, ask, onChanged }: Props) {
  const [editing, setEditing] = useState(false);
  const [values, setValues] = useState<SpawnValues>(() => spawnValues(grant));
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<FleetResult | null>(null);
  const save = () => {
    if (ask === undefined) return;
    setBusy(true);
    void saveSpawn(grant, values, actions, ask)
      .then((r) => {
        if (r === null) return;
        setResult(r);
        if (r.ok) setEditing(false);
        onChanged();
      })
      .finally(() => setBusy(false));
  };
  return (
    <div className="fd-grant-desktop">
      {grant.state === 'revoked' || ask === undefined || editing ? null : (
        <button
          type="button"
          onClick={() => {
            setValues(spawnValues(grant));
            setResult(null);
            setEditing(true);
          }}
        >
          Change session rules
        </button>
      )}
      {editing ? (
        <SpawnForm
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
    </div>
  );
}
