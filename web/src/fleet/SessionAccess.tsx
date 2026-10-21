// The "Fleet access…" dialog of the session menu: give, change, renew or revoke the coordinator
// grant of one session. It reuses the logic of the Fleet panel.
import { useEffect, useState } from 'react';
import type { DeckSession, GrantView, StationView } from '../gen';
import { Dialog } from '../integrations/Dialog';
import { useSettings } from '../settings/context';
import type { ElevationAsk } from '../settings/elevation';
import { FleetControlFields } from '../spawn/FleetControlFields';
import { type FleetControl, fleetProblem } from '../spawn/fleetControl';
import { useFleetElevation } from '../spawn/useFleetElevation';
import { type FleetActions, type FleetResult, grantStatus, NOT_LIVE } from './fleet';
import { CODEX_NOTE, ChangeAccess, giveAccess, START_LIVE } from './GrantAccess';
import { grantTitle } from './GrantSpawn';

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

type BodyProps = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  /** `null`: the grants are loading. */
  readonly grant: GrantView | undefined | null;
  readonly actions: FleetActions;
  /** The elevation of the owner; `undefined` for a login that cannot change access. */
  readonly ask: ElevationAsk | undefined;
  readonly fleet: FleetControl;
  readonly busy: boolean;
  readonly result: FleetResult | null;
  readonly confirmRevoke: boolean;
  readonly onFleet: (next: FleetControl) => void;
  readonly onGive: () => void;
  /** The first "Revoke" click: the body asks to confirm. */
  readonly onRevoke: () => void;
  /** The "Revoke" click of the confirm: the grant is revoked. */
  readonly onConfirmRevoke: () => void;
  readonly onCancelRevoke: () => void;
  readonly onRenew: () => void;
  readonly onChanged: () => void;
};

/** The body of the dialog, without the load: it shows the grant, or the form that gives one. */
export function SessionAccessBody({
  session,
  stations,
  grant,
  actions,
  ask,
  fleet,
  busy,
  result,
  confirmRevoke,
  onFleet,
  onGive,
  onRevoke,
  onConfirmRevoke,
  onCancelRevoke,
  onRenew,
  onChanged,
}: BodyProps) {
  const codex = session.summary.provider === 'codex';
  const liveStation = stations.some(
    (s) => s.station_id === session.station_id && s.coordinator_live === true,
  );
  const problem = fleetProblem(fleet);
  return (
    <>
      {grant === null ? (
        <p role="status" className="fd-note">
          Loading…
        </p>
      ) : grant === undefined ? (
        liveStation && !NOT_LIVE.includes(session.status) ? (
          <form
            className="fd-fleet-form"
            aria-label="Give fleet access"
            onSubmit={(event) => {
              event.preventDefault();
              onGive();
            }}
          >
            <p className="fd-note">{NO_ACCESS}</p>
            <FleetControlFields fleet={fleet} onChange={onFleet} live />
            {codex ? <p className="fd-note">{CODEX_NOTE}</p> : null}
            {ask === undefined ? null : (
              <div className="fd-fleet-actions">
                <button type="submit" disabled={busy || problem !== null}>
                  Give access
                </button>
              </div>
            )}
          </form>
        ) : (
          <p className="fd-note">{CANNOT_GIVE}</p>
        )
      ) : (
        <div className="fd-fleet-form">
          <p>
            <span className="fd-task-title">{grantTitle(grant)}</span>{' '}
            <span className={`fd-task-state fd-grant-${grant.state}`}>{grantStatus(grant)}</span>
          </p>
          {grant.state === 'expired' && grant.revoked_reason === 'idle' ? (
            <div className="fd-fleet-actions">
              <button type="button" disabled={busy} onClick={onRenew}>
                Renew
              </button>
            </div>
          ) : null}
          {grant.live === true ? (
            <ChangeAccess
              key={grant.grant_id}
              grant={grant}
              actions={actions}
              ask={ask}
              codex={codex}
              onChanged={onChanged}
            />
          ) : null}
          {confirmRevoke ? (
            <>
              <p className="fd-note">Revoke fleet access of this session?</p>
              <div className="fd-fleet-actions">
                <button
                  type="button"
                  className="fd-danger"
                  disabled={busy}
                  onClick={onConfirmRevoke}
                >
                  Revoke
                </button>
                <button type="button" disabled={busy} onClick={onCancelRevoke}>
                  Cancel
                </button>
              </div>
            </>
          ) : (
            <div className="fd-fleet-actions">
              <button type="button" className="fd-danger" disabled={busy} onClick={onRevoke}>
                Revoke
              </button>
            </div>
          )}
        </div>
      )}
      {result === null ? null : (
        <p className="fd-note" role={result.ok ? 'status' : 'alert'}>
          {result.text}
        </p>
      )}
    </>
  );
}

type Props = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  readonly actions: FleetActions;
  readonly onClose: () => void;
};

/** Gives, changes, renews or revokes the coordinator grant of one session. */
export function SessionAccessDialog({ session, stations, actions, onClose }: Props) {
  const settingsEnv = useSettings();
  const { ask: askElevation, node } = useFleetElevation(settingsEnv);
  const ask = settingsEnv === undefined ? undefined : askElevation;
  const [grant, setGrant] = useState<GrantView | undefined | null>(null);
  const [failed, setFailed] = useState(false);
  const [version, setVersion] = useState(0);
  const [fleet, setFleet] = useState<FleetControl>(START_LIVE);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<FleetResult | null>(null);
  const [confirmRevoke, setConfirmRevoke] = useState(false);
  useEffect(() => {
    let current = true;
    // `version` runs the load again after an action.
    void version;
    actions.grants().then(
      (all) => {
        if (!current) return;
        setFailed(false);
        setGrant(sessionGrant(all, session));
      },
      () => {
        if (current) setFailed(true);
      },
    );
    return () => {
      current = false;
    };
  }, [actions, session, version]);
  const reload = () => setVersion((n) => n + 1);
  const act = (call: () => Promise<FleetResult | null>, done?: () => void) => {
    if (busy) return;
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
        <p className="fd-note" role="alert">
          {LOAD_FAILED}
        </p>
      ) : (
        <SessionAccessBody
          session={session}
          stations={stations}
          grant={grant}
          actions={actions}
          ask={ask}
          fleet={fleet}
          busy={busy}
          result={result}
          confirmRevoke={confirmRevoke}
          onFleet={setFleet}
          onGive={() => {
            if (ask === undefined || fleetProblem(fleet) !== null) return;
            act(
              () => giveAccess(session, fleet, actions, ask),
              () => setFleet(START_LIVE),
            );
          }}
          onRevoke={() => setConfirmRevoke(true)}
          onConfirmRevoke={() => {
            if (grant === null || grant === undefined) return;
            setConfirmRevoke(false);
            act(() => actions.revoke(grant.grant_id));
          }}
          onCancelRevoke={() => setConfirmRevoke(false)}
          onRenew={() => {
            if (grant === null || grant === undefined) return;
            act(() => actions.renew(grant.grant_id));
          }}
          onChanged={reload}
        />
      )}
      <div className="fd-fleet-actions">
        <button type="button" onClick={onClose}>
          Close
        </button>
      </div>
      {node}
    </Dialog>
  );
}
