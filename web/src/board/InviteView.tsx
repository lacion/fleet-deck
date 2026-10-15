import { useEffect, useState } from 'react';
import type { PairingInvitation, StationView } from '../gen';
import { clockText, countdownText } from '../tailnet/model';
import { CommandBlock, CopyButton, WaitLine } from '../tailnet/CommandBlock';
import { useNow } from '../ui/useNow';
import type { StationActions } from './pairing';
import { Steps } from './Steps';
import { useRun } from './useRun';

const STEPS = ['Create', 'Run', 'Done'] as const;
const MASK = '••••••••••••';

type Props = {
  readonly actions: StationActions;
  readonly stations: readonly StationView[];
  /** The invitation is a secret: the dialog keeps it in memory and drops it on close. */
  readonly invitation: PairingInvitation | null;
  readonly setInvitation: (invitation: PairingInvitation | null) => void;
};

/** The station dials this deck: a one-time invitation, the join command, and the wait. */
export function InviteView({ actions, stations, invitation, setInvitation }: Props) {
  const { busy, error, run } = useRun();
  const [known, setKnown] = useState<readonly string[]>([]);
  const [joined, setJoined] = useState<string | null>(null);
  const [revealed, setRevealed] = useState(false);
  const now = useNow(undefined, 1_000);

  // A station that is new since the invitation was made is the one that used it.
  useEffect(() => {
    if (invitation === null) return;
    const fresh = stations.find((s) => !known.includes(s.station_id));
    if (fresh === undefined) return;
    setJoined(fresh.name);
    setRevealed(false);
    setInvitation(null);
  }, [stations, known, invitation, setInvitation]);

  const create = () =>
    run(async () => {
      const made = await actions.invite();
      setKnown(stations.map((s) => s.station_id));
      setJoined(null);
      setRevealed(false);
      setInvitation(made);
    });

  const expired = invitation !== null && invitation.expires_at <= now;
  const step = joined !== null ? 2 : invitation !== null ? 1 : 0;

  return (
    <section className="fd-add-flow" aria-label="Any computer, with a one-time invitation">
      <Steps labels={STEPS} current={step} done={joined !== null} />
      {joined !== null ? (
        <>
          <p className="fd-add-done" role="status">
            ✓ {joined} joined this deck.
          </p>
          <button type="button" onClick={() => setJoined(null)}>
            Add another station
          </button>
        </>
      ) : invitation === null ? (
        <>
          <p>
            Create a one-time invitation. It works for one station only, and it ends after a short
            time.
          </p>
          <div>
            <button type="button" className="fd-primary" disabled={busy} onClick={create}>
              Create invitation
            </button>
          </div>
        </>
      ) : expired ? (
        <>
          <p role="status">The invitation ended.</p>
          <div>
            <button type="button" disabled={busy} onClick={create}>
              Create a new invitation
            </button>
          </div>
        </>
      ) : (
        <>
          <p>1. On the new computer, run:</p>
          <CommandBlock command={invitation.join_command} />
          <p>2. When it asks, paste this invitation:</p>
          <div className="fd-add-command">
            {revealed ? (
              <textarea
                readOnly
                rows={3}
                aria-label="Invitation (secret, single use)"
                value={invitation.invitation}
                onFocus={(event) => event.currentTarget.select()}
              />
            ) : (
              <span className="fd-add-mask" aria-hidden="true">
                {MASK}
              </span>
            )}
            <div className="fd-add-command-actions">
              <CopyButton text={invitation.invitation} label="Copy invitation" />
              <button type="button" onClick={() => setRevealed((shown) => !shown)}>
                {revealed ? 'Hide' : 'Show'}
              </button>
            </div>
          </div>
          <p className="fd-note">
            Keep it secret. Anyone with it can add a station until{' '}
            {clockText(invitation.expires_at)}.
          </p>
          <p className="fd-note">Ends in {countdownText(invitation.expires_at - now)}</p>
          <WaitLine>Waiting for the station to join…</WaitLine>
        </>
      )}
      {error === null ? null : <p role="alert">{error}</p>}
    </section>
  );
}
