// The fleet access of the ops panel (MCP P4b-4, DESIGN-SYSTEM §4.4): the grants that are not revoked,
// one row each with "Change…", which opens the "Fleet access" dialog of the session menu for the
// grant (renew, change, revoke; also for a grant whose session is not on the board); and "Give
// fleet access…", a dialog with the session and the same fields. Giving access is rare: the column
// shows only the rows and one quiet button, never a form. The expired grants wait in a collapsed
// "Expired" fold (the look of a lane's Ended fold): they are nothing to act on until a renewal.
import { type ReactNode, useState } from 'react';
import { stationName } from '../board/fleet';
import type { DeckSession, GrantView, StationView } from '../gen';
import { Dialog } from '../integrations/Dialog';
import { useSettings } from '../settings/context';
import { useFleetElevation } from '../spawn/useFleetElevation';
import { Icon, Plus, ShieldCheck } from '../ui/icons';
import { type FleetActions, type FleetResult, grantStations, grantStatus } from './fleet';
import { GiveAccess, giveCandidates, titleOf } from './GrantAccess';
import { SessionAccessDialog } from './SessionAccess';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly grants: readonly GrantView[];
  readonly actions: FleetActions;
  /**
   * `section`: a section of the wide ops column, with an `.fd-ops-h` heading. `tab`: a block of
   * the Mail & tasks tab (under 1280 px and on the phone), with an `.fd-label` heading.
   */
  readonly layout: 'section' | 'tab';
  /** Called after a dialog changed or may have changed a grant: the grants load again. */
  readonly onChanged: () => void;
};

const keyOf = (s: DeckSession) => `${s.station_id}/${s.summary.session_id}`;

/**
 * The state of a grant as a row says it, in words (never a raw reason id): a given grant whose
 * session has not bound it yet waits; a grant that expired from idle time says so.
 */
export function grantState(g: GrantView): string {
  if (g.state === 'active') return g.generation === null ? 'waiting for its session' : 'active';
  if (g.state === 'expired')
    return g.revoked_reason === 'idle' ? 'expired after idle time' : 'expired';
  if (g.state === 'revoked') {
    if (g.revoked_reason === 'station_updated') return 'ended by a station update';
    if (g.revoked_reason === 'station_reset') return 'ended: the station has new data';
    if (g.revoked_reason === 'station_removed') return 'ended: the station was removed';
  }
  return g.state;
}

const sentence = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

const RANK: Readonly<Record<string, number>> = { active: 0, expired: 1 };

/**
 * The grants that the column lists: every grant that is not revoked, the active ones first. A row
 * is short (the session, the kind and the state); its "Fleet access" dialog has the details.
 */
export function accessRows(grants: readonly GrantView[]): readonly GrantView[] {
  return grants
    .filter((g) => g.state !== 'revoked')
    .map((g, i) => ({ g, i }))
    .sort((a, b) => (RANK[a.g.state] ?? 2) - (RANK[b.g.state] ?? 2) || a.i - b.i)
    .map((x) => x.g);
}

const kindWord = (g: GrantView) => (g.kind === 'worker' ? 'Worker' : 'Coordinator');

/**
 * The words of a grant row. A grant whose session is on the board: the session's title over the
 * kind and the state ("Worker · active"). One whose session is not on the board (not started yet,
 * or gone) names its kind and station ("Coordinator on Lighting") over its state alone, so the
 * kind is not said twice.
 */
export function grantRowText(
  g: GrantView,
  session: DeckSession | undefined,
  stations: readonly StationView[],
): { readonly title: string; readonly sub: string } {
  if (session !== undefined)
    return { title: titleOf(session), sub: `${kindWord(g)} · ${grantState(g)}` };
  const title =
    g.station_id === ''
      ? `New ${kindWord(g).toLowerCase()}`
      : `${kindWord(g)} on ${stationName(g.station_id, stations)}`;
  return { title, sub: sentence(grantState(g)) };
}

/** The dialog of "Give fleet access…": its own elevation, as the session menu's dialog. */
function GiveDialog({
  sessions,
  stations,
  grants,
  actions,
  onClose,
  onGiven,
}: {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly grants: readonly GrantView[];
  readonly actions: FleetActions;
  readonly onClose: () => void;
  readonly onGiven: (result: FleetResult) => void;
}) {
  const settingsEnv = useSettings();
  const { ask, node } = useFleetElevation(settingsEnv);
  return (
    <Dialog title="Give fleet access" onClose={onClose}>
      <GiveAccess
        sessions={sessions}
        stations={stations}
        grants={grants}
        actions={actions}
        ask={ask}
        onChanged={onGiven}
        onClose={onClose}
      />
      {node}
    </Dialog>
  );
}

/** The grants of the fleet as rows, and the way to give a running session fleet access. */
export function FleetAccess({ sessions, stations, grants, actions, layout, onChanged }: Props) {
  const owner = useSettings() !== undefined;
  const [giving, setGiving] = useState(false);
  // The grant of the open "Fleet access" dialog and the key of its session ('' when the session
  // is not on the board: the dialog shows the grant by its id).
  const [open, setOpen] = useState<{ readonly grant: string; readonly key: string } | null>(null);
  const [result, setResult] = useState<FleetResult | null>(null);
  const rows = accessRows(grants);
  // The grants that still give access are the rows; the expired ones fold away (as a lane's Ended
  // strips), and the heading counts only the rows it shows.
  const current = rows.filter((g) => g.state !== 'expired');
  const expired = rows.filter((g) => g.state === 'expired');
  const canGive =
    owner &&
    stations.some((s) => s.coordinator_live === true) &&
    giveCandidates(sessions, stations, grants).length > 0;
  const sessionOf = (g: GrantView) =>
    sessions.find((s) => s.station_id === g.station_id && s.summary.session_id === g.session_id);
  const opened = open === null ? undefined : sessions.find((s) => keyOf(s) === open.key);
  if (rows.length === 0 && !canGive && !giving && open === null) return null;
  const count = current.length === 0 ? null : <span className="fd-count">{current.length}</span>;
  const heading =
    layout === 'section' ? (
      <h2 className="fd-ops-h">
        <Icon of={ShieldCheck} size={14} /> Fleet access
        {count}
      </h2>
    ) : (
      <h3 className="fd-label fd-access-tab-h">
        Fleet access
        {count}
      </h3>
    );
  const row = (g: GrantView): ReactNode => {
    const session = sessionOf(g);
    const text = grantRowText(g, session, stations);
    const where = grantStations(g, stations);
    const id = `fd-grant-${g.grant_id}`;
    return (
      <li key={g.grant_id} title={where === '' ? grantStatus(g) : `${grantStatus(g)}\n${where}`}>
        <span className="fd-ops-row-text">
          <span className="fd-ops-row-title" id={id}>
            {text.title}
          </span>
          <span className="fd-ops-row-sub" data-state={g.state}>
            {text.sub}
          </span>
        </span>
        <button
          type="button"
          className="fd-ghost fd-sm"
          aria-describedby={id}
          onClick={() => {
            setResult(null);
            setOpen({
              grant: g.grant_id,
              key: session === undefined ? '' : keyOf(session),
            });
          }}
        >
          Change…
        </button>
      </li>
    );
  };
  return (
    <section
      className={layout === 'section' ? 'fd-ops-sec fd-ops-access' : 'fd-fleet-access'}
      aria-label="Fleet access"
    >
      {heading}
      {current.length === 0 ? null : (
        <ul className="fd-ops-rows fd-access-list" aria-label="Coordinator grants">
          {current.map(row)}
        </ul>
      )}
      {expired.length === 0 ? null : (
        <details className="fd-access-expired">
          <summary>
            Expired <span className="fd-count">{expired.length}</span>
          </summary>
          <ul className="fd-ops-rows fd-access-list" aria-label="Expired grants">
            {expired.map(row)}
          </ul>
        </details>
      )}
      {canGive ? (
        <button
          type="button"
          className="fd-fleet-give"
          onClick={() => {
            setResult(null);
            setGiving(true);
          }}
        >
          <Icon of={Plus} size={14} /> Give fleet access…
        </button>
      ) : null}
      {result === null ? null : (
        <p className="fd-note" role="status">
          {result.text}
        </p>
      )}
      {giving ? (
        <GiveDialog
          sessions={sessions}
          stations={stations}
          grants={grants}
          actions={actions}
          onClose={() => setGiving(false)}
          onGiven={(r) => {
            setGiving(false);
            setResult(r);
            onChanged();
          }}
        />
      ) : null}
      {open === null ? null : (
        <SessionAccessDialog
          session={opened}
          grantId={open.grant}
          stations={stations}
          actions={actions}
          onClose={() => {
            setOpen(null);
            onChanged();
          }}
        />
      )}
    </section>
  );
}
