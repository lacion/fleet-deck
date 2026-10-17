// FG-9 (design §12.2, §12.4A, §12.7, §12.8): the texts and the rules of the board side of a move.
// Pure functions: the dialog, the banner and the composer gate share them.
import { formatHash } from '../app/routes';
import { stationName } from '../board/fleet';
import type { MoveInState, ProviderKind, SessionSummary, StationView } from '../gen';
import type { MoveTarget, MoveView } from '../integrations/actions';

/** How often an open move is read again (decision H26, Q4). */
export const MOVE_POLL_MS = 2000;

const ENDED = ['done', 'failed', 'undone'];

/** A move that still changes: every state except the three ends. */
export const isOpenMove = (move: MoveView | null): boolean =>
  move !== null && !ENDED.includes(move.state);

const MIB = 1024 * 1024;

/** "12.5" for 13 107 200 bytes. */
export const mib = (bytes: number): string => (bytes / MIB).toFixed(1);

export const SOURCE_RUNNING =
  'The source session runs again. Stop it, then Fleet Deck finishes the move.';

/** The progress line of a move (§12.8), `null` when the move has no progress text. */
export function progressText(move: MoveView, dst: string): string | null {
  switch (move.state) {
    case 'holding':
      return 'Moving: holding';
    case 'exporting':
    case 'importing':
      return `Moving: copying the conversation${
        move.bytes === undefined ? '' : ` (${mib(move.bytes)} MiB)`
      }`;
    case 'starting':
      return `Moving: starting on ${dst}`;
    case 'committed':
      return 'Moving: finishing';
    case 'finishing':
      return move.code === 'source_running' ? SOURCE_RUNNING : 'Moving: finishing';
    case 'releasing':
      return 'Moving: releasing the new session';
    case 'aborting':
      return 'Cancelling the move…';
    case 'undoing':
      return 'Undoing the move…';
    case 'failed':
      return move.message ?? move.code ?? null;
    default:
      return null;
  }
}

/** The six conditions of §12.2 that the board shows, in the order that the deck checks them. */
export const CHECKS = [
  'No turn is running',
  'No approval is open',
  'The branch is pushed',
  'The target has a home of this project',
  'No other move is open',
  'The session has confirmed its conversation',
] as const;

const CODES_OF_CHECK_4 = [
  'fleet_git_unsupported',
  'home_not_ready',
  'home_unverified',
  'slug_taken',
  'station_info_missing',
];

/** Which condition (0 to 5) a refusal of the deck belongs to, from its fixed texts and codes. */
export function failingCheck(failed: {
  readonly code: string;
  readonly text: string;
}): number | null {
  const { code, text } = failed;
  if (text === 'A turn is running.') return 0;
  if (text === 'An approval is open.') return 1;
  if (text === 'The branch is not pushed.' || text === 'This session is not in a synced project.') {
    return 2;
  }
  if (
    text.endsWith(' has no home of this project.') ||
    text === 'The session is already on this station.' ||
    text.endsWith(' is offline.') ||
    CODES_OF_CHECK_4.includes(code)
  ) {
    return 3;
  }
  if (text === 'The session is already moving.' || text === 'The session is being landed.') {
    return 4;
  }
  if (text === 'The session has not confirmed its conversation yet. Send it one prompt first.') {
    return 5;
  }
  return null;
}

export type Mark = 'passed' | 'failed' | 'neutral';

export type CheckItem = {
  readonly label: string;
  readonly mark: Mark;
  /** The text of the deck, on the failed item. */
  readonly text?: string;
};

export type Checklist = {
  readonly items: readonly CheckItem[];
  /** The refusal text of the deck when no item matches it. */
  readonly note: string | null;
};

/**
 * The checklist of the chosen target. The deck stops at the first failed condition: the items
 * before it passed, the items after it were not checked. No target, or a refusal that no item
 * matches: every item is neutral.
 */
export function checklist(target: MoveTarget | undefined): Checklist {
  const all = (mark: Mark): readonly CheckItem[] => CHECKS.map((label) => ({ label, mark }));
  if (target === undefined) return { items: all('neutral'), note: null };
  if (target.failed === undefined) {
    return { items: all(target.ready ? 'passed' : 'neutral'), note: null };
  }
  const at = failingCheck(target.failed);
  if (at === null) return { items: all('neutral'), note: target.failed.text };
  const { text } = target.failed;
  return {
    items: CHECKS.map((label, i): CheckItem => {
      if (i < at) return { label, mark: 'passed' };
      return i === at ? { label, mark: 'failed', text } : { label, mark: 'neutral' };
    }),
    note: null,
  };
}

/** The kind line of the dialog (§12.8). */
export const kindLine = (kind: string): string =>
  kind === 'native'
    ? 'The conversation moves with its history.'
    : 'Only the branch moves; a new session continues from the last summary.';

const providerText = (kind: ProviderKind | null): string =>
  kind === 'claude' ? 'Claude' : kind === 'codex' ? 'Codex' : 'The provider';

/** The states of `move_in` in which the new session has no composer. */
const HOLDING: readonly MoveInState[] = [
  'staged',
  'imported',
  'launching',
  'launch_refused',
  'resumed',
  'resume_failed',
  'failed',
  'abandoned',
  'discarded',
  'unknown',
];

/** The composer is hidden on a moved source and on a target that no move has released yet. */
export function composerHidden(s: SessionSummary): boolean {
  if (s.moved_to !== undefined) return true;
  return s.move_in !== undefined && HOLDING.includes(s.move_in.state);
}

/** "Send one prompt to confirm the moved conversation." shows above a released Claude composer. */
export const confirmLineShown = (s: SessionSummary): boolean =>
  s.move_in?.state === 'released' && s.provider === 'claude';

export const CONFIRM_LINE = 'Send one prompt to confirm the moved conversation.';

/** What the banner shows: a line, its tone, and the action (an "Open" link or "Undo move"). */
export type Notice = {
  readonly tone: 'info' | 'hazard';
  readonly text: string;
  readonly action:
    | { readonly kind: 'open'; readonly href: string }
    | { readonly kind: 'undo'; readonly move: string }
    | null;
  /** A failed move stays until the person dismisses it. */
  readonly dismissible: boolean;
  /** The lines of an open move are announced as a status. */
  readonly key: string;
};

type Where = {
  readonly station: string;
  readonly summary: SessionSummary;
  readonly move: MoveView | null;
  readonly stations: readonly StationView[];
};

const info = (key: string, text: string, action: Notice['action'] = null): Notice => ({
  tone: 'info',
  text,
  action,
  dismissible: false,
  key,
});

/** The target card or the source header of a move (§12.4A, §12.7, §12.8), `null` for none. */
export function moveNotice({ station, summary: s, move, stations }: Where): Notice | null {
  const own = (id: string) => stationName(id, stations);
  if (move?.state === 'undoing') return info('undoing', 'Undoing the move…');
  const src = move === null ? 'the source station' : own(move.src_station);
  const isSource =
    move !== null && move.src_station === station && move.src_session === s.session_id;
  if (s.moved_to !== undefined) {
    const to = own(s.moved_to.station_id);
    if (move?.state === 'resume_failed' && move.move_id === s.moved_to.move_id) {
      return info('resume_failed', `Moved to ${to}. The new session could not resume.`, {
        kind: 'undo',
        move: move.move_id,
      });
    }
    return info('moved', `Moved to ${to}`, {
      kind: 'open',
      href: formatHash({
        name: 'session',
        station: s.moved_to.station_id,
        session: s.moved_to.session_id,
        tab: 'chat',
      }),
    });
  }
  if (s.move_in !== undefined) return targetNotice({ station, summary: s, move, stations }, src);
  if (isSource && move !== null) {
    const text = progressText(move, own(move.dst_station));
    if (text === null) return null;
    if (move.state === 'failed') {
      return {
        tone: 'hazard',
        text,
        action: null,
        dismissible: true,
        key: `failed-${move.move_id}`,
      };
    }
    return info(`open-${move.state}`, text);
  }
  return null;
}

function targetNotice({ station, summary: s, stations }: Where, src: string): Notice | null {
  const m = s.move_in;
  if (m === undefined) return null;
  switch (m.state) {
    case 'staged':
    case 'imported':
    case 'launching':
    case 'resumed':
      return info('arriving', `Arriving from ${src}…`);
    case 'resume_failed': {
      const line = m.text === undefined || m.text === '' ? '' : `: ${m.text}`;
      return info(
        'resume_failed',
        `${providerText(s.provider)} could not resume the conversation on ${stationName(station, stations)}${line}. The conversation is still on ${src}, read only. Undo the move to continue there.`,
        { kind: 'undo', move: m.move_id },
      );
    }
    case 'failed':
      return info('undone', `This move was undone. The session continues on ${src}.`);
    case 'abandoned':
      return info(
        'abandoned',
        `This copy of the conversation was not used. The session stays on ${src}.`,
      );
    case 'discarded':
      return info('discarded', 'This move was cancelled.');
    case 'launch_refused':
    case 'unknown':
      return m.text === undefined || m.text === '' ? null : info(m.state, m.text);
    default:
      return null;
  }
}
