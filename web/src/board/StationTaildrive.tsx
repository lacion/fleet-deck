// FG-12 (design §15.7): the Taildrive part of a station card: the shares, the warning of an exposed
// share, and the notes of a share that is missing, ambiguous or not fully checked; and the
// Taildrive fact of the Details fold when there is no share list.
import type { Exposure, StationInfo, TaildriveShare } from '../gen';

/** What an exposure kind names in the warning (design §15.7). */
const WHAT: Readonly<Record<string, string>> = {
  project_home: 'a project home',
  worktree: 'a session worktree',
  data_dir: 'the Fleet Deck data folder',
  claude_home: 'the Claude home',
  codex_home: 'the Codex home',
  ssh: '~/.ssh',
  gnupg: '~/.gnupg',
  aws: '~/.aws',
  member_deny: 'a member-hidden path',
};

const UNKNOWN_WHAT = 'an unknown path';

/** The warning of an exposed share, or `null` when it exposes nothing. */
export function exposedText(
  share: Pick<TaildriveShare, 'exposed' | 'exposure_state'>,
): string | null {
  const kinds: readonly Exposure[] = Array.isArray(share.exposed) ? share.exposed : [];
  if (kinds.length === 0 && share.exposure_state !== 'exposed') return null;
  const what =
    kinds.length === 0 ? UNKNOWN_WHAT : kinds.map((k) => WHAT[k] ?? UNKNOWN_WHAT).join(', ');
  return `This Taildrive share exposes ${what}. Devices in your tailnet policy can read it, outside Fleet Deck's file rules.`;
}

/** The note lines of one share, after its row. */
export function shareNotes(share: TaildriveShare): { text: string; warn: boolean }[] {
  const notes: { text: string; warn: boolean }[] = [];
  const exposed = exposedText(share);
  if (exposed !== null) notes.push({ text: exposed, warn: true });
  if (share.path_state === 'missing') {
    notes.push({ text: `${share.name}: the path does not exist on this station.`, warn: false });
  }
  if (share.path_state === 'ambiguous') {
    notes.push({
      text: `${share.name}: Fleet Deck cannot tell the exact path (it may end with spaces). Add it to [taildrive_mounts] to use it for artifacts.`,
      warn: false,
    });
  }
  if (share.exposure_state === 'unknown') {
    const why = share.unknown_reason === undefined ? '' : ` (${share.unknown_reason})`;
    notes.push({
      text: `${share.name}: Fleet Deck could not check all files of this share${why}. It cannot be an artifact share.`,
      warn: true,
    });
  }
  return notes;
}

/**
 * The Taildrive fact of the Details fold when the card body has no share list: a share list that
 * cannot be read, no Taildrive on the station, or no share. The station's own reason (a tool
 * error such as "tailscale drive list failed (exit 1)") is only the tooltip. An older station
 * sends no `taildrive`, and a station with shares lists them in the body: no fact.
 */
export function taildriveFact(
  taildrive: StationInfo['taildrive'],
): { readonly text: string; readonly reason?: string | undefined } | null {
  if (taildrive === undefined) return null;
  if (taildrive.state === 'unavailable') return { text: 'Not available', reason: taildrive.reason };
  if (taildrive.state === 'unknown') {
    return { text: 'Share list not readable', reason: taildrive.reason };
  }
  if (taildrive.state === 'ok' && taildrive.shares.length === 0) return { text: 'No shares' };
  return null;
}

/** The Taildrive row of the Details facts (`<dt>` + `<dd>`), or nothing. */
export function TaildriveFact({ taildrive }: { readonly taildrive: StationInfo['taildrive'] }) {
  const fact = taildriveFact(taildrive);
  if (fact === null) return null;
  return (
    <>
      <dt>Taildrive</dt>
      <dd className="fd-facts-text" title={fact.reason}>
        {fact.text}
      </dd>
    </>
  );
}

/**
 * The Taildrive shares of a station card, with the warning of an exposed share. Only a report
 * with shares has a part in the card body: a station without Taildrive, or without a share, has
 * nothing to show there (principle 7; the Details fold says why, `TaildriveFact`).
 */
export function StationTaildrive({ taildrive }: { readonly taildrive: StationInfo['taildrive'] }) {
  if (taildrive === undefined || taildrive.state !== 'ok' || taildrive.shares.length === 0) {
    return null;
  }
  return (
    <div
      className="fd-station-prop fd-station-taildrive"
      role="group"
      aria-label="Taildrive shares"
    >
      <h4 className="fd-station-prop-label">Taildrive shares</h4>
      <ul className="fd-station-shares">
        {taildrive.shares.map((share) => (
          <li key={share.name} data-share={share.name}>
            <span>
              {share.name} · {share.path}
              {share.as_user === undefined ? '' : ` · as ${share.as_user}`}
            </span>
            {shareNotes(share).map((n) => (
              <p
                key={n.text}
                className="fd-wt-note"
                data-tone={n.warn ? 'act' : undefined}
                role={n.warn ? 'alert' : undefined}
              >
                {n.text}
              </p>
            ))}
          </li>
        ))}
      </ul>
    </div>
  );
}
