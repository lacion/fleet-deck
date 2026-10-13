// R11.4 (DL-166): "Shares" (owner only): every share with its members, uses and expiry, and Revoke.
import { useEffect, useState } from 'react';
import { sessionTitle } from '../app/sessions';
import { stationName } from '../board/fleet';
import type { DeckSession, ShareInfo, StationView } from '../gen';
import { useNow } from '../ui/useNow';
import { canShare, usePeople } from './context';
import { roleLabel } from './member';
import { day } from './ShareDialog';
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
    <li className="fd-shares-row" data-state={state}>
      <div className="fd-shares-main">
        <span className="fd-badge" data-tone="info">
          {share.kind}
        </span>
        <span className="fd-shares-target">{where}</span>
        <span className="fd-badge" data-tone={share.role === 'viewer' ? 'muted' : 'act'}>
          {roleLabel(share.role)}
        </span>
        {share.role === 'collaborator' ? (
          <span className="fd-badge" data-tone="muted">
            {share.steering === 'mob' ? 'mob' : 'open'}
          </span>
        ) : null}
        {state === 'live' ? null : (
          <span className="fd-badge" data-tone="muted">
            {state === 'revoked' ? 'Revoked' : 'Expired'}
          </span>
        )}
      </div>
      <p className="fd-note fd-shares-facts">
        {share.members.length === 0 ? 'No members yet' : `Members: ${share.members.join(', ')}`}
        {' · '}
        {share.uses_left} {share.uses_left === 1 ? 'use' : 'uses'} left
        {' · '}
        {state === 'revoked'
          ? `revoked ${day(share.revoked_at ?? 0)}`
          : `expires ${day(share.expires_at)}`}
      </p>
      {state === 'live' ? (
        confirming ? (
          <div className="fd-shares-confirm" role="group" aria-label="Revoke the share">
            <p className="fd-note">Revoke this share? The members lose access at once.</p>
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
          </div>
        ) : (
          <button
            type="button"
            className="fd-ghost fd-sm fd-danger"
            aria-label={`Revoke the share of ${where}`}
            onClick={onAsk}
          >
            Revoke
          </button>
        )
      ) : null}
    </li>
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
}: ViewProps) {
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
      <p className="fd-empty">
        No shares yet. Use Share… in a session menu, on the fleet home or on a space.
      </p>
    );
  } else {
    body = (
      <ul className="fd-shares-list" aria-label="Shares">
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
      </ul>
    );
  }
  return (
    <section className="fd-shares" aria-label="Shares">
      <h1 className="fd-screen-title">Shares</h1>
      <p className="fd-note">
        People join through a link. A revoke ends the link and every login that came from it. See{' '}
        <a href="#/spaces">Spaces</a> for sets of sessions.
      </p>
      {error === null ? null : (
        <p role="alert" className="fd-note fd-share-error">
          {error}
        </p>
      )}
      {body}
    </section>
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
  const [confirming, setConfirming] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    // A new share elsewhere (the dialog) shows here after the next load.
    setConfirming(null);
  }, [version]);
  if (!canShare(people) || calls === undefined) {
    return (
      <div className="fd-screen">
        <p>Only the owner of the deck sees the shares.</p>
        <a href="#/">Back to the fleet</a>
      </div>
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
    <div className="fd-screen">
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
      />
    </div>
  );
}
