// FG-1..FG-4 (design §4.4, §5.7, §6.4, §7.4): the timeline lines of the Fleet Git events
// `workspace.base_stale`, `workspace.include_skipped`, `session.push` and `session.cleanup`.
import type { Event, PushInfo, PushState } from '../gen';
import type { Item } from './timeline';

type Notice = Extract<Item, { type: 'notice' }>;

/** The title of "Push stopped", by `stop_code` (§5.9, decision 19). */
export function stoppedTitle(push: Pick<PushInfo, 'stop_code' | 'oid'>): string | undefined {
  switch (push.stop_code) {
    case 'land_published':
      return 'The land pushed a rebased branch. New commits on this branch are not pushed automatically.';
    case 'project_gone': {
      const since = push.oid === undefined ? 'the start' : push.oid.slice(0, 7);
      return `Sync is off for this project. Commits after ${since} are not pushed.`;
    }
    case 'unsupported_mode':
      return 'This station does not know the push mode of the project. Update the station.';
    default:
      return undefined;
  }
}

/** What the lines of one timeline remember: the push state before, and the last cleanup line. */
export type FleetGitMemory = { push: PushState | null; cleanup: string | null };

export const newFleetGitMemory = (): FleetGitMemory => ({ push: null, cleanup: null });

/** The reason texts of `workspace.include_skipped` (§7.4); an unknown reason shows its code. */
const SKIP_REASON: Readonly<Record<string, string>> = {
  symlink: 'a link',
  not_regular: 'not a regular file',
  too_large: 'larger than 1 MiB',
  total_too_large: 'over the 10 MiB total',
  exists: 'already in the worktree',
  unreadable: 'not readable',
  not_ignored: 'not ignored by Git',
  too_many_matches: 'more than 200 matches',
};

/** The most files that one `workspace.include_skipped` lists (§3.6). */
const MAX_SKIPPED = 50;

/** The card menu texts of the cleanup reasons (§6.4), without the clause of a missing value. */
const KEPT: Readonly<Record<string, string>> = {
  held: 'Kept: a land or a move holds this session.',
  detached: 'Kept: the worktree is not on a branch (detached HEAD).',
  branch_changed: 'Kept: the worktree is on another branch.',
  changed: 'Kept: the worktree changed after the cleanup checks.',
  fetch_failed: 'Kept: the fetch failed; cleanup tries again later.',
  not_merged:
    'Kept: the branch is not merged. A squash merge is not detected; trash the worktree yourself.',
  dirty: 'Kept: the worktree has changes or new files.',
  not_pushed: 'Kept: the last commit is not pushed.',
  no_default_branch: 'Kept: the default branch of the remote is unknown.',
  scan_unknown: 'Kept: the worktree scan did not finish.',
  git_too_old: 'Kept: Fleet Git needs Git 2.45 or newer on this station.',
};

/** The text of a failing push when the station sent none (§5.7). */
const FAILING: Readonly<Record<string, string>> = {
  push_timeout: 'Push took more than 60 s.',
  git_too_old: 'Fleet Git needs Git 2.45 or newer on this station.',
};

/** The text of a cleanup reason; `trash_refused` may carry the refusal text after a colon. */
export function keptText(reason: string): string {
  const known = KEPT[reason];
  if (known !== undefined) return known;
  if (reason.startsWith('trash_refused')) {
    const text = reason.slice('trash_refused'.length).replace(/^:\s*/, '').trim();
    return text === '' ? 'Kept.' : `Kept: ${text}`;
  }
  return `Kept: ${reason}`;
}

/**
 * The timeline line of a Fleet Git event, or `null` (not such an event, or no line for this
 * state). `memory` is updated.
 */
export function fleetGitNotice(
  event: Event | { kind: string },
  key: string,
  memory: FleetGitMemory,
): Notice | null {
  const line = (text: string, tone: Notice['tone']): Notice => ({
    type: 'notice',
    key,
    text,
    tone,
  });
  switch (event.kind) {
    case 'workspace.base_stale': {
      const e = event as Extract<Event, { kind: 'workspace.base_stale' }>;
      return line(e.text, 'info');
    }
    case 'workspace.include_skipped': {
      const e = event as Extract<Event, { kind: 'workspace.include_skipped' }>;
      if (e.files.length === 0) return null;
      const text = e.files
        .slice(0, MAX_SKIPPED)
        .map((f) => `Not copied: ${f.path} (${SKIP_REASON[f.reason] ?? f.reason})`)
        .join('\n');
      return line(text, 'info');
    }
    case 'session.push': {
      const e = event as Extract<Event, { kind: 'session.push' }>;
      const before = memory.push;
      memory.push = e.state;
      if (e.state === 'failing') {
        const text = e.text ?? (e.code === undefined ? undefined : FAILING[e.code]);
        return line(text ?? `Push failed${e.code === undefined ? '.' : `: ${e.code}`}`, 'error');
      }
      if (e.state === 'stopped') {
        const text = e.text ?? stoppedTitle(e) ?? e.stop_code ?? '';
        return line(`Push stopped: ${text}`, 'error');
      }
      if (e.state === 'pushed' && (before === 'failing' || before === 'stopped')) {
        return line(`Pushed ${(e.oid ?? '').slice(0, 7)}`, 'info');
      }
      return null;
    }
    case 'session.cleanup': {
      const e = event as Extract<Event, { kind: 'session.cleanup' }>;
      if (e.state === 'trashed') {
        memory.cleanup = null;
        return line('Moved to the trash by cleanup. Restore it from Stations → Worktrees.', 'info');
      }
      if (e.state !== 'kept' || e.reason === undefined) return null;
      const text = keptText(e.reason);
      if (text === memory.cleanup) return null;
      memory.cleanup = text;
      return line(text, 'info');
    }
    default:
      return null;
  }
}
