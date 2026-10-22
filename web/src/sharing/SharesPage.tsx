// R11.4 (DL-166): "Shares" (owner only): every share with its members, uses and expiry, and Revoke.
import { useEffect, useState } from 'react';
import { sessionTitle } from '../app/sessions';
import { stationName } from '../board/fleet';
import type { DeckSession, ShareInfo, ShareKind, ShareRole, SpaceView, StationView } from '../gen';
import { untilText } from '../automations/automations';
import { useSettings } from '../settings/context';
import type { ElevationAsk } from '../settings/elevation';
import { useFleetElevation } from '../spawn/useFleetElevation';
import {
  Icon,
  Layers,
  LayoutGrid,
  type LucideIcon,
  MessageSquare,
  Plus,
  Share2,
} from '../ui/icons';
import { EmptyState, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import { canShare, usePeople } from './context';
import type { ShareCalls } from './people';
import { day, NewShareDialog } from './ShareDialog';
import { sharesChanged, useSharesVersion } from './shareStore';
import { type Loaded, useLoad } from './useLoad';

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/** DL-189: the text before the owner turns the history option on. */
const HISTORY_WARNING =
  'Members see the full earlier conversation of the shared sessions, including anything that was pasted into it. Tool commands and file names stay hidden.';

/** DL-189: turns the history option on. It needs the elevation first; `null` when the owner cancels. */
export async function shareHistoryOn(
  calls: Pick<ShareCalls, 'setHistory'>,
  ask: ElevationAsk,
  shareId: string,
): Promise<ShareInfo | null> {
  const token = await ask(['Share the earlier history']);
  if (token === null) return null;
  return calls.setHistory(shareId, true, token);
}

/** DL-189: turns the history option off at once (no confirm, no elevation). */
export function shareHistoryOff(
  calls: Pick<ShareCalls, 'setHistory'>,
  shareId: string,
): Promise<ShareInfo> {
  return calls.setHistory(shareId, false);
}

/** The state of a share: `revoked`, `expired` (time or no uses and no members), or `live`. */
export function shareState(share: ShareInfo, now: number): 'revoked' | 'expired' | 'live' {
  if (share.revoked_at !== null) return 'revoked';
  return share.expires_at <= now ? 'expired' : 'live';
}

/** The kind of a share in sentence case, with its icon (§5: no raw enum values). */
const KIND: Readonly<Record<ShareKind, { readonly label: string; readonly icon: LucideIcon }>> = {
  session: { label: 'Session', icon: MessageSquare },
  space: { label: 'Space', icon: Layers },
  board: { label: 'Board', icon: LayoutGrid },
};

/** The role of a share in sentence case. */
const ROLE: Readonly<Record<ShareRole, string>> = {
  viewer: 'Viewer',
  collaborator: 'Collaborator',
  approver: 'Approver',
};

/** What a share covers, in words. */
export function targetText(
  share: ShareInfo,
  sessions: readonly DeckSession[],
  stations: readonly StationView[],
): string {
  if (share.kind === 'board') return 'The whole board';
  if (share.kind === 'space') return `Space ${share.target}`;
  const [station = '', session = ''] = share.target.split('/');
  const card = sessions.find((s) => s.station_id === station && s.summary.session_id === session);
  return `${stationName(station, stations)} / ${card === undefined ? session : sessionTitle(card)}`;
}

type RowProps = {
  readonly share: ShareInfo;
  readonly now: number;
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly confirming: boolean;
  readonly busy: boolean;
  /** DL-189: the confirm of "Share history" is open on this row. */
  readonly historyConfirming: boolean;
  /** DL-189: a history change of this row is in progress. */
  readonly historyBusy: boolean;
  readonly onAsk: () => void;
  readonly onCancel: () => void;
  readonly onRevoke: () => void;
  readonly onHistoryAsk: () => void;
  readonly onHistory: () => void;
  readonly onStopHistory: () => void;
};

/** The expiry of a share, relative to now: `in 5 days`, `revoked 2026-10-13`, `expired 2026-10-12`. */
function expiryText(share: ShareInfo, state: ReturnType<typeof shareState>, now: number): string {
  if (state === 'revoked') return `revoked ${day(share.revoked_at ?? 0)}`;
  if (state === 'expired') return `expired ${day(share.expires_at)}`;
  return untilText(share.expires_at, now);
}

function ShareRow({
  share,
  now,
  sessions,
  stations,
  confirming,
  busy,
  historyConfirming,
  historyBusy,
  onAsk,
  onCancel,
  onRevoke,
  onHistoryAsk,
  onHistory,
  onStopHistory,
}: RowProps) {
  const state = shareState(share, now);
  const where = targetText(share, sessions, stations);
  return (
    <tr data-state={state} data-dim={state === 'live' ? undefined : 'true'}>
      <td>
        <span className="fd-cellbadges">
          <span className="fd-badge">
            <Icon of={KIND[share.kind].icon} size={12} />
            {KIND[share.kind].label}
          </span>
          <span className="fd-cell-name">{where}</span>
        </span>
      </td>
      <td data-label="Role">
        <span className="fd-cellbadges">
          <span>{ROLE[share.role]}</span>
          {share.role !== 'viewer' ? (
            <span className="fd-badge" data-tone="muted">
              {share.steering === 'mob' ? 'Mob' : 'Open'}
            </span>
          ) : null}
          {share.history ? <span className="fd-badge">History shared</span> : null}
          {state === 'live' ? null : (
            <span className="fd-badge" data-tone="muted">
              {state === 'revoked' ? 'Revoked' : 'Expired'}
            </span>
          )}
        </span>
      </td>
      <td data-label="Members">
        {share.members.length === 0 ? 'No members yet' : share.members.join(', ')}
      </td>
      <td data-label="Uses left" className="fd-num">
        {share.uses_left}
      </td>
      <td
        data-label="Expires"
        title={day(state === 'revoked' ? (share.revoked_at ?? 0) : share.expires_at)}
      >
        {expiryText(share, state, now)}
      </td>
      <td className="fd-cell-actions">
        {state !== 'live' ? null : confirming ? (
          <span className="fd-share-confirm" role="group" aria-label="Revoke the share">
            <span>Revoke this share? The members lose access at once.</span>
            <button
              type="button"
              className="fd-danger fd-sm"
              disabled={busy}
              aria-label={`Revoke the share of ${where}`}
              onClick={onRevoke}
            >
              {busy ? 'Revoking…' : 'Revoke'}
            </button>
            <button type="button" className="fd-ghost fd-sm" disabled={busy} onClick={onCancel}>
              Cancel
            </button>
          </span>
        ) : historyConfirming ? (
          <span className="fd-share-confirm" role="group" aria-label="Share the earlier history">
            <span>{HISTORY_WARNING}</span>
            <button
              type="button"
              className="fd-sm"
              disabled={historyBusy}
              aria-label={`Share history of ${where}`}
              onClick={onHistory}
            >
              Share history
            </button>
            <button
              type="button"
              className="fd-ghost fd-sm"
              disabled={historyBusy}
              aria-label={`Cancel sharing history of ${where}`}
              onClick={onCancel}
            >
              Cancel
            </button>
          </span>
        ) : (
          <>
            {share.history ? (
              <button
                type="button"
                className="fd-ghost fd-sm"
                disabled={historyBusy}
                aria-label={`Stop sharing history of ${where}`}
                onClick={onStopHistory}
              >
                Stop sharing history
              </button>
            ) : (
              <button
                type="button"
                className="fd-ghost fd-sm"
                disabled={historyBusy}
                aria-label={`Share history of ${where}`}
                onClick={onHistoryAsk}
              >
                Share history
              </button>
            )}
            <button
              type="button"
              className="fd-ghost fd-sm fd-danger"
              aria-label={`Revoke the share of ${where}`}
              onClick={onAsk}
            >
              Revoke
            </button>
          </>
        )}
      </td>
    </tr>
  );
}

type ViewProps = {
  readonly state: Loaded<readonly ShareInfo[]>;
  readonly now: number;
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  /** The share that waits for the confirm. */
  readonly confirming?: string | null;
  readonly revoking?: string | null;
  readonly error?: string | null;
  readonly onRetry?: () => void;
  readonly onAsk?: (id: string | null) => void;
  readonly onRevoke?: (share: ShareInfo) => void;
  /** DL-189: the share whose history change is in progress (a share id). */
  readonly historyBusy?: string | null;
  /** DL-189: confirmed "Share history" (the page asks for the elevation). */
  readonly onHistory?: (share: ShareInfo) => void;
  /** DL-189: "Stop sharing history". */
  readonly onStopHistory?: (share: ShareInfo) => void;
  /** "New share": opens the dialog with a target select. */
  readonly onNew?: () => void;
};

/** The list of one state: loading, error with "Try again", empty, or the rows. */
export function SharesView({
  state,
  now,
  sessions,
  stations,
  confirming = null,
  revoking = null,
  error = null,
  onRetry,
  onAsk,
  onRevoke,
  historyBusy = null,
  onHistory,
  onStopHistory,
  onNew,
}: ViewProps) {
  const newShare = (
    <button type="button" className="fd-primary" onClick={onNew}>
      <Icon of={Plus} /> New share
    </button>
  );
  let body: React.ReactNode;
  if (state.kind === 'loading') {
    body = (
      <p role="status" className="fd-note">
        Loading the shares…
      </p>
    );
  } else if (state.kind === 'error') {
    body = (
      <div className="fd-art-state">
        <p role="alert" className="fd-note fd-share-error">
          {state.text}
        </p>
        <button type="button" className="fd-ghost fd-sm" onClick={onRetry}>
          Try again
        </button>
      </div>
    );
  } else if (state.value.length === 0) {
    body = (
      <EmptyState
        icon={Share2}
        title="No shares yet"
        body="Share a session, a space or the whole board with a link. People join with a name you can see, in the role you pick."
        action={newShare}
      />
    );
  } else {
    body = (
      <div className="fd-panel fd-tablepanel">
        <div className="fd-tablewrap">
          <table className="fd-table" aria-label="Shares">
            <thead>
              <tr>
                <th scope="col">Shared</th>
                <th scope="col">Role</th>
                <th scope="col">Members</th>
                <th scope="col" className="fd-num">
                  Uses left
                </th>
                <th scope="col">Expires</th>
                <th scope="col">
                  <span className="fd-vh">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {state.value.map((share) => (
                <ShareRow
                  key={share.share_id}
                  share={share}
                  now={now}
                  sessions={sessions}
                  stations={stations}
                  confirming={confirming === share.share_id}
                  busy={revoking === share.share_id}
                  historyConfirming={confirming === `history:${share.share_id}`}
                  historyBusy={historyBusy === share.share_id}
                  onAsk={() => onAsk?.(share.share_id)}
                  onCancel={() => onAsk?.(null)}
                  onRevoke={() => onRevoke?.(share)}
                  onHistoryAsk={() => onAsk?.(`history:${share.share_id}`)}
                  onHistory={() => onHistory?.(share)}
                  onStopHistory={() => onStopHistory?.(share)}
                />
              ))}
            </tbody>
          </table>
        </div>
      </div>
    );
  }
  return (
    <Page
      title="Shares"
      description={
        <>
          People join through a link. A revoke ends the link and every login that came from it. See{' '}
          <a className="fd-tap" href="#/spaces">
            Spaces
          </a>{' '}
          for sets of sessions.
        </>
      }
      actions={newShare}
    >
      {error === null ? null : (
        <p role="alert" className="fd-note fd-share-error">
          {error}
        </p>
      )}
      {body}
    </Page>
  );
}

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
};

/** The Shares page. */
export function SharesPage({ sessions, stations }: Props) {
  const people = usePeople();
  const calls = people.shares;
  const version = useSharesVersion();
  const now = useNow(undefined, 30_000);
  const { state, reload } = useLoad<readonly ShareInfo[]>(
    () => (calls === undefined ? Promise.resolve([]) : calls.shares()),
    [calls, version],
  );
  const spaces = useLoad<readonly SpaceView[]>(
    () => (calls === undefined ? Promise.resolve([]) : calls.spaces()),
    [calls],
  );
  const [confirming, setConfirming] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [making, setMaking] = useState(false);
  const [working, setWorking] = useState<string | null>(null);
  const { ask, node: elevationNode } = useFleetElevation(useSettings());
  useEffect(() => {
    // A new share elsewhere (the dialog) shows here after the next load.
    setConfirming(null);
  }, [version]);
  if (!canShare(people) || calls === undefined) {
    return (
      <Page title="Shares">
        <p>Only the owner of the deck sees the shares.</p>
        <a href="#/">Back to the fleet</a>
      </Page>
    );
  }
  const revoke = async (share: ShareInfo) => {
    setRevoking(share.share_id);
    setError(null);
    try {
      await calls.revoke(share.share_id);
      setConfirming(null);
      sharesChanged();
    } catch (e) {
      setError(message(e));
    } finally {
      setRevoking(null);
    }
  };
  // DL-189: turn the history option on (after the confirm and the elevation) or off.
  const changeHistory = async (share: ShareInfo, on: boolean) => {
    setWorking(share.share_id);
    setError(null);
    try {
      const done = on
        ? await shareHistoryOn(calls, ask, share.share_id)
        : await shareHistoryOff(calls, share.share_id);
      if (done !== null) {
        setConfirming(null);
        sharesChanged();
      }
    } catch (e) {
      setError(message(e));
    } finally {
      setWorking(null);
    }
  };
  return (
    <>
      <SharesView
        state={state}
        now={now}
        sessions={sessions}
        stations={stations}
        confirming={confirming}
        revoking={revoking}
        error={error}
        onRetry={reload}
        onAsk={setConfirming}
        onRevoke={(share) => void revoke(share)}
        historyBusy={working}
        onHistory={(share) => void changeHistory(share, true)}
        onStopHistory={(share) => void changeHistory(share, false)}
        onNew={() => setMaking(true)}
      />
      {making ? (
        <NewShareDialog
          sessions={sessions}
          stations={stations}
          spaces={spaces.state.kind === 'ok' ? spaces.state.value : []}
          calls={calls}
          onClose={() => setMaking(false)}
        />
      ) : null}
      {elevationNode}
    </>
  );
}
