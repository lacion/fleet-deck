// R11.4 (DL-166): "Shares" (owner only): every share with its members, uses and expiry, and Revoke.
import { useEffect, useState } from 'react';
import { sessionTitle } from '../app/sessions';
import { stationName } from '../board/fleet';
import type { DeckSession, ShareInfo, SpaceView, StationView } from '../gen';
import { untilText } from '../automations/automations';
import { Icon, Plus, Share2 } from '../ui/icons';
import { EmptyState, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import { canShare, usePeople } from './context';
import { roleLabel } from './member';
import { day, NewShareDialog } from './ShareDialog';
import { sharesChanged, useSharesVersion } from './shareStore';
import { type Loaded, useLoad } from './useLoad';

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/** The state of a share: `revoked`, `expired` (time or no uses and no members), or `live`. */
export function shareState(share: ShareInfo, now: number): 'revoked' | 'expired' | 'live' {
  if (share.revoked_at !== null) return 'revoked';
  return share.expires_at <= now ? 'expired' : 'live';
}

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
  readonly onAsk: () => void;
  readonly onCancel: () => void;
  readonly onRevoke: () => void;
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
  onAsk,
  onCancel,
  onRevoke,
}: RowProps) {
  const state = shareState(share, now);
  const where = targetText(share, sessions, stations);
  return (
    <tr data-state={state} data-dim={state === 'live' ? undefined : 'true'}>
      <td>
        <span className="fd-cellbadges">
          <span className="fd-badge" data-tone="info">
            {share.kind}
          </span>
          <span className="fd-cell-name">{where}</span>
        </span>
      </td>
      <td data-label="Role">
        <span className="fd-cellbadges">
          <span className="fd-badge" data-tone={share.role === 'viewer' ? 'muted' : 'act'}>
            {roleLabel(share.role)}
          </span>
          {share.role !== 'viewer' ? (
            <span className="fd-badge" data-tone="muted">
              {share.steering === 'mob' ? 'mob' : 'open'}
            </span>
          ) : null}
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
      <td className="fd-cell-path" data-label="Uses left">
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
          <span role="group" aria-label="Revoke the share">
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
        ) : (
          <button
            type="button"
            className="fd-ghost fd-sm fd-danger"
            aria-label={`Revoke the share of ${where}`}
            onClick={onAsk}
          >
            Revoke
          </button>
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
                <th scope="col">Uses left</th>
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
                  onAsk={() => onAsk?.(share.share_id)}
                  onCancel={() => onAsk?.(null)}
                  onRevoke={() => onRevoke?.(share)}
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
      wide
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
    </>
  );
}
