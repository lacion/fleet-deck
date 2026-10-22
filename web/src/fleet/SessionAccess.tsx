// The "Fleet access…" dialog of the session menu: give, change, renew or revoke the coordinator
// grant of one session. It reuses the logic of the Fleet panel.
import { useEffect, useRef, useState } from 'react';
import { stationName } from '../board/fleet';
import type { DeckSession, GrantView, StationView } from '../gen';
import { Dialog } from '../integrations/Dialog';
import { useSettings } from '../settings/context';
import type { ElevationAsk } from '../settings/elevation';
import { type FleetControl, fleetProblem, NO_FLEET } from '../spawn/fleetControl';
import { useFleetElevation } from '../spawn/useFleetElevation';
import { AccessFields } from './AccessForm';
import { type FleetActions, type FleetResult, grantStatus, NOT_LIVE } from './fleet';
import { GrantDesktop } from './GrantDesktop';
import { GrantSpawn, grantTitle, hasCeilings } from './GrantSpawn';
import { ceilingLabel } from './modes';
import {
  type AccessValues,
  accessProblem,
  accessValues,
  EDITABLE,
  giveAccess,
  saveAccess,
  START_LIVE,
  titleOf,
} from './GrantAccess';

const NO_ACCESS = 'This session has no fleet access.';
const CANNOT_GIVE =
  'This session cannot get fleet access now: its station cannot change grants live, or the session is not running.';
const LOAD_FAILED = 'The fleet access of this session could not be loaded.';

/** The grant of the session that is not revoked; with several, the last one in the list. */
export function sessionGrant(
  grants: readonly GrantView[],
  session: DeckSession,
): GrantView | undefined {
  return grants
    .filter(
      (g) =>
        g.station_id === session.station_id &&
        g.session_id === session.summary.session_id &&
        g.state !== 'revoked',
    )
    .at(-1);
}

/** The values of the permissions form before a grant is loaded into it. */
const NO_VALUES: AccessValues = {
  classes: ['read', 'mail', 'drive'],
  spawnCap: NO_FLEET.spawnCap,
  claude: NO_FLEET.claudeCeiling,
  codex: NO_FLEET.codexCeiling,
  answerCap: NO_FLEET.answerCap,
};

/** The values of the permissions form, as the fields of the give form show them. */
function editFleet(v: AccessValues): FleetControl {
  return {
    ...NO_FLEET,
    spawn: v.classes.includes('spawn'),
    inspect: v.classes.includes('inspect'),
    manage: v.classes.includes('manage'),
    answer: v.classes.includes('answer'),
    spawnCap: v.spawnCap,
    answerCap: v.answerCap,
    claudeCeiling: v.claude,
    codexCeiling: v.codex,
  };
}

/** The values of the permissions form after a field changed: the checked editable classes and the limits. */
function editValues(f: FleetControl): AccessValues {
  const on: Record<string, boolean> = {
    spawn: f.spawn,
    inspect: f.inspect,
    manage: f.manage,
    answer: f.answer,
  };
  return {
    classes: EDITABLE.filter((c) => on[c] ?? true),
    spawnCap: f.spawnCap,
    claude: f.claudeCeiling,
    codex: f.codexCeiling,
    answerCap: f.answerCap,
  };
}

type BodyProps = {
  /** `undefined`: a grant whose session is not on the board (opened from its row by its id). */
  readonly session: DeckSession | undefined;
  readonly stations: readonly StationView[];
  /** `null`: the grants are loading. */
  readonly grant: GrantView | undefined | null;
  readonly actions: FleetActions;
  /** The elevation of the owner; `undefined` for a login that cannot change access. */
  readonly ask: ElevationAsk | undefined;
  readonly fleet: FleetControl;
  /** The picked stations of the give form. */
  readonly picked: readonly string[];
  readonly busy: boolean;
  readonly result: FleetResult | null;
  readonly confirmRevoke: boolean;
  /** True while the permissions form of the grant shows. */
  readonly editing: boolean;
  /** The values of the permissions form. */
  readonly values: AccessValues;
  readonly onFleet: (next: FleetControl) => void;
  readonly onPicked: (ids: readonly string[]) => void;
  readonly onValues: (next: AccessValues) => void;
  readonly onGive: () => void;
  /** The first "Revoke" click: the body asks to confirm. */
  readonly onRevoke: () => void;
  /** The "Revoke" click of the confirm: the grant is revoked. */
  readonly onConfirmRevoke: () => void;
  readonly onCancelRevoke: () => void;
  readonly onRenew: () => void;
  readonly onEdit: () => void;
  readonly onCancelEdit: () => void;
  readonly onSave: () => void;
  readonly onClose: () => void;
  /** A change of the session rules or the desktop limits: the grant loads again. */
  readonly onChanged: () => void;
};

function Status({ result }: { readonly result: FleetResult | null }) {
  return result === null ? null : (
    <p className="fd-note" role={result.ok ? 'status' : 'alert'}>
      {result.text}
    </p>
  );
}

/** The footer with "Close" only. */
function CloseFoot({ onClose }: { readonly onClose: () => void }) {
  return (
    <div className="fd-access-foot">
      <button type="button" className="fd-ghost" onClick={onClose}>
        Close
      </button>
    </div>
  );
}

/** The body of the dialog, without the load: it shows the grant, or the form that gives one. */
export function SessionAccessBody({
  session,
  stations,
  grant,
  actions,
  ask,
  fleet,
  picked,
  busy,
  result,
  confirmRevoke,
  editing,
  values,
  onFleet,
  onPicked,
  onValues,
  onGive,
  onRevoke,
  onConfirmRevoke,
  onCancelRevoke,
  onRenew,
  onEdit,
  onCancelEdit,
  onSave,
  onClose,
  onChanged,
}: BodyProps) {
  const codex = session?.summary.provider === 'codex';
  // The station and the name of the session; a grant without its session says its own.
  const own = session?.station_id ?? (grant == null ? '' : grant.station_id);
  const name =
    session === undefined ? (grant == null ? '' : grantTitle(grant).trim()) : titleOf(session);
  const liveStation = stations.some((s) => s.station_id === own && s.coordinator_live === true);
  const sub = (
    <p className="fd-access-sub">
      {name} · {stationName(own, stations)}
    </p>
  );
  if (grant === null) {
    return (
      <>
        {sub}
        <p role="status" className="fd-note">
          Loading…
        </p>
        <CloseFoot onClose={onClose} />
      </>
    );
  }
  if (grant === undefined) {
    // The outcome of a revoke stays in view: the revoked grant is no longer the session's.
    if (session === undefined) {
      return (
        <>
          <Status result={result} />
          <p className="fd-note">{NO_ACCESS}</p>
          <CloseFoot onClose={onClose} />
        </>
      );
    }
    if (!liveStation || NOT_LIVE.includes(session.status)) {
      return (
        <>
          {sub}
          <Status result={result} />
          <p className="fd-note">{CANNOT_GIVE}</p>
          <CloseFoot onClose={onClose} />
        </>
      );
    }
    const problem = fleetProblem(fleet);
    return (
      <>
        {sub}
        <p className="fd-note">{NO_ACCESS}</p>
        <form
          className="fd-fleet-form"
          aria-label="Give fleet access"
          onSubmit={(event) => {
            event.preventDefault();
            if (ask !== undefined && !busy && problem === null) onGive();
          }}
        >
          <AccessFields
            stations={stations}
            ownId={session.station_id}
            picked={picked}
            onPicked={onPicked}
            fleet={fleet}
            onFleet={onFleet}
            codex={codex}
          />
          <Status result={result} />
          <div className="fd-access-foot">
            <button type="button" className="fd-ghost" onClick={onClose}>
              Close
            </button>
            {ask === undefined ? null : (
              <button type="submit" className="fd-primary" disabled={busy || problem !== null}>
                Give access
              </button>
            )}
          </div>
        </form>
      </>
    );
  }
  if (editing) {
    const problem = accessProblem(grant, values);
    return (
      <>
        {sub}
        <form
          className="fd-fleet-form"
          aria-label="Change permissions"
          onSubmit={(event) => {
            event.preventDefault();
            if (!busy && problem === null) onSave();
          }}
        >
          <AccessFields
            stations={stations}
            ownId={own}
            picked={grant.stations.filter((id) => id !== own)}
            onPicked={() => {}}
            fleet={editFleet(values)}
            onFleet={(f) => onValues(editValues(f))}
            codex={codex}
            stationsLocked
            lockedClasses={grant.classes}
          />
          <Status result={result} />
          <div className="fd-access-foot">
            <button type="button" className="fd-ghost" disabled={busy} onClick={onCancelEdit}>
              Cancel
            </button>
            <button type="submit" className="fd-primary" disabled={busy || problem !== null}>
              Save
            </button>
          </div>
        </form>
      </>
    );
  }
  const shown = [
    ...grant.stations.filter((id) => id === own),
    ...grant.stations.filter((id) => id !== own),
  ];
  const max = grant.max_modes ?? {};
  const modes = [
    ...(max['claude'] === undefined ? [] : [`Claude: ${ceilingLabel(max['claude'])}`]),
    ...(max['codex'] === undefined ? [] : [`Codex: ${ceilingLabel(max['codex'])}`]),
  ];
  return (
    <>
      {sub}
      <dl className="fd-access-sum">
        <dt>Status</dt>
        <dd>{grantStatus(grant)}</dd>
        <dt>Permissions</dt>
        <dd>
          {grant.classes.map((c) => (
            <span key={c} className="fd-chip">
              {c}
            </span>
          ))}
        </dd>
        <dt>Stations</dt>
        <dd>{shown.map((id) => stationName(id, stations)).join(', ')}</dd>
        {grant.classes.includes('spawn') ? (
          <>
            <dt>Sessions started</dt>
            <dd>{`${grant.spawn_used} of ${grant.spawn_cap}`}</dd>
          </>
        ) : null}
        {modes.length === 0 ? null : (
          <>
            <dt>Highest modes</dt>
            <dd>{modes.join(' · ')}</dd>
          </>
        )}
        <dt>Expires</dt>
        <dd>{new Date(grant.expires_at).toLocaleString()}</dd>
      </dl>
      {/* The session rules and the desktop limits of the grant (they moved here from the rows of
          the ops panel, which only open this dialog). */}
      {grant.state === 'revoked' ||
      (!hasCeilings(grant) && grant.desktop_cap === undefined) ? null : (
        <div className="fd-access-more">
          {hasCeilings(grant) ? (
            <GrantSpawn grant={grant} actions={actions} ask={ask} onChanged={onChanged} />
          ) : null}
          {grant.desktop_cap === undefined ? null : (
            <GrantDesktop
              grant={grant}
              stations={stations}
              actions={actions}
              ask={ask}
              onChanged={onChanged}
            />
          )}
        </div>
      )}
      <Status result={result} />
      {confirmRevoke ? (
        <>
          <p className="fd-note">Revoke fleet access of this session?</p>
          <div className="fd-fleet-actions">
            <button type="button" className="fd-danger" disabled={busy} onClick={onConfirmRevoke}>
              Revoke
            </button>
            <button type="button" disabled={busy} onClick={onCancelRevoke}>
              Cancel
            </button>
          </div>
        </>
      ) : (
        <div className="fd-access-foot">
          <button type="button" className="fd-ghost" onClick={onClose}>
            Close
          </button>
          {grant.state === 'expired' && grant.revoked_reason === 'idle' ? (
            <button type="button" disabled={busy} onClick={onRenew}>
              Renew
            </button>
          ) : null}
          {grant.live === true && ask !== undefined ? (
            <button type="button" disabled={busy} onClick={onEdit}>
              Change permissions
            </button>
          ) : null}
          <button type="button" className="fd-danger" disabled={busy} onClick={onRevoke}>
            Revoke
          </button>
        </div>
      )}
    </>
  );
}

type Props = {
  /** The session; `undefined` for a grant whose session is not on the board (with `grantId`). */
  readonly session: DeckSession | undefined;
  /** The grant to show (a row of the ops panel); absent: the grant of the session. */
  readonly grantId?: string | undefined;
  readonly stations: readonly StationView[];
  readonly actions: FleetActions;
  readonly onClose: () => void;
};

/** The grant that the dialog shows: the one of `grantId` while it is not revoked, else the
 * session's grant. */
export function dialogGrant(
  grants: readonly GrantView[],
  session: DeckSession | undefined,
  grantId: string | undefined,
): GrantView | undefined {
  if (grantId !== undefined)
    return grants.find((g) => g.grant_id === grantId && g.state !== 'revoked');
  return session === undefined ? undefined : sessionGrant(grants, session);
}

/** Gives, changes, renews or revokes the coordinator grant of one session. */
export function SessionAccessDialog({ session, grantId, stations, actions, onClose }: Props) {
  const settingsEnv = useSettings();
  const { ask: askElevation, node } = useFleetElevation(settingsEnv);
  const ask = settingsEnv === undefined ? undefined : askElevation;
  const [grant, setGrant] = useState<GrantView | undefined | null>(null);
  const [failed, setFailed] = useState(false);
  const [version, setVersion] = useState(0);
  const [fleet, setFleet] = useState<FleetControl>(START_LIVE);
  const [picked, setPicked] = useState<readonly string[]>([]);
  const [editing, setEditing] = useState(false);
  const [values, setValues] = useState<AccessValues>(NO_VALUES);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<FleetResult | null>(null);
  const [confirmRevoke, setConfirmRevoke] = useState(false);
  // The load follows the session's key, not each new copy of the session (every fleet event).
  const sessionRef = useRef(session);
  sessionRef.current = session;
  const key = session === undefined ? '' : `${session.station_id}/${session.summary.session_id}`;
  useEffect(() => {
    let current = true;
    // `key` and `version` run the load again (another session, an action).
    void key;
    void version;
    actions.grants().then(
      (all) => {
        if (!current) return;
        setFailed(false);
        setGrant(dialogGrant(all, sessionRef.current, grantId));
      },
      () => {
        if (current) setFailed(true);
      },
    );
    return () => {
      current = false;
    };
  }, [actions, key, grantId, version]);
  const reload = () => setVersion((n) => n + 1);
  const act = (call: () => Promise<FleetResult | null>, done?: () => void) => {
    if (busy) return;
    setResult(null);
    setBusy(true);
    void call()
      .then((r) => {
        if (r === null) return;
        setResult(r);
        if (r.ok) done?.();
        reload();
      })
      .finally(() => setBusy(false));
  };
  return (
    <Dialog title="Fleet access" onClose={onClose}>
      {failed ? (
        <>
          <p className="fd-note" role="alert">
            {LOAD_FAILED}
          </p>
          <CloseFoot onClose={onClose} />
        </>
      ) : (
        <SessionAccessBody
          session={session}
          stations={stations}
          grant={grant}
          actions={actions}
          ask={ask}
          fleet={fleet}
          picked={picked}
          busy={busy}
          result={result}
          confirmRevoke={confirmRevoke}
          editing={editing}
          values={values}
          onFleet={setFleet}
          onPicked={setPicked}
          onValues={setValues}
          onGive={() => {
            if (session === undefined || ask === undefined || fleetProblem(fleet) !== null) return;
            const targets = picked.filter((id) => id !== session.station_id);
            act(
              () => giveAccess(session, fleet, actions, ask, targets),
              () => {
                setFleet(START_LIVE);
                setPicked([]);
              },
            );
          }}
          onRevoke={() => {
            setResult(null);
            setConfirmRevoke(true);
          }}
          onConfirmRevoke={() => {
            if (grant === null || grant === undefined) return;
            setConfirmRevoke(false);
            setEditing(false);
            act(() => actions.revoke(grant.grant_id));
          }}
          onCancelRevoke={() => setConfirmRevoke(false)}
          onRenew={() => {
            if (grant === null || grant === undefined) return;
            act(() => actions.renew(grant.grant_id));
          }}
          onEdit={() => {
            if (grant === null || grant === undefined) return;
            setResult(null);
            setValues(accessValues(grant));
            setEditing(true);
          }}
          onCancelEdit={() => setEditing(false)}
          onSave={() => {
            if (ask === undefined || grant === null || grant === undefined) return;
            if (accessProblem(grant, values) !== null) return;
            act(
              () => saveAccess(grant, values, actions, ask),
              () => setEditing(false),
            );
          }}
          onClose={onClose}
          onChanged={reload}
        />
      )}
      {node}
    </Dialog>
  );
}
