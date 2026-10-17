// FG-9 (design §12.2, §12.4A, §12.7, §12.8): the texts and rules of the board side of a move.
import { describe, expect, test } from 'bun:test';
import type { MoveInState } from '../gen';
import type { MoveState, MoveTarget, MoveView } from '../integrations/actions';
import { station } from '../testing/fleet';
import { summary } from '../testing/summary';
import {
  CHECKS,
  checklist,
  composerHidden,
  confirmLineShown,
  failingCheck,
  isOpenMove,
  kindLine,
  mib,
  moveNotice,
  progressText,
  SOURCE_RUNNING,
} from './moveModel';

const move = (state: MoveState, extra: Partial<MoveView> = {}): MoveView => ({
  move_id: 'm1',
  state,
  kind: 'native',
  src_station: 'station-a',
  src_session: 's1',
  dst_station: 'station-b',
  dst_session: 's2',
  created_at: 1,
  ...extra,
});

const stations = [station('station-a'), station('station-b')];

describe('the progress line', () => {
  test('each state has the text of §12.8', () => {
    const text = (state: MoveState, extra: Partial<MoveView> = {}) =>
      progressText(move(state, extra), 'Beta');
    expect(text('holding')).toBe('Moving: holding');
    expect(text('exporting', { bytes: 13_107_200 })).toBe(
      'Moving: copying the conversation (12.5 MiB)',
    );
    expect(text('importing')).toBe('Moving: copying the conversation');
    expect(text('starting')).toBe('Moving: starting on Beta');
    expect(text('committed')).toBe('Moving: finishing');
    expect(text('finishing')).toBe('Moving: finishing');
    expect(text('releasing')).toBe('Moving: releasing the new session');
    expect(text('aborting')).toBe('Cancelling the move…');
    expect(text('undoing')).toBe('Undoing the move…');
  });

  test('a finish that waits for a running source says so', () => {
    expect(progressText(move('finishing', { code: 'source_running' }), 'Beta')).toBe(
      SOURCE_RUNNING,
    );
    expect(SOURCE_RUNNING).toBe(
      'The source session runs again. Stop it, then Fleet Deck finishes the move.',
    );
  });

  test('a failed move shows the text of the deck, else its code; the quiet states show none', () => {
    expect(progressText(move('failed', { code: 'move_lost', message: 'It stopped.' }), 'B')).toBe(
      'It stopped.',
    );
    expect(progressText(move('failed', { code: 'move_lost' }), 'B')).toBe('move_lost');
    expect(progressText(move('failed'), 'B')).toBeNull();
    for (const state of ['done', 'undone', 'resume_failed'] as const) {
      expect(progressText(move(state), 'B')).toBeNull();
    }
  });

  test('MiB has one decimal and the three ends close the move', () => {
    expect(mib(1_048_576)).toBe('1.0');
    expect(mib(0)).toBe('0.0');
    expect(isOpenMove(null)).toBe(false);
    for (const state of ['done', 'failed', 'undone'] as const) {
      expect(isOpenMove(move(state))).toBe(false);
    }
    for (const state of ['holding', 'resume_failed', 'undoing', 'finishing'] as const) {
      expect(isOpenMove(move(state))).toBe(true);
    }
  });
});

describe('the checklist', () => {
  test('the fixed texts and codes of the deck find their condition', () => {
    const at = (text: string, code = 'move_not_ready') => failingCheck({ code, text });
    expect(at('A turn is running.')).toBe(0);
    expect(at('An approval is open.')).toBe(1);
    expect(at('The branch is not pushed.')).toBe(2);
    expect(at('This session is not in a synced project.')).toBe(2);
    expect(at('Beta has no home of this project.')).toBe(3);
    expect(at('The session is already on this station.')).toBe(3);
    expect(at('Beta is offline.')).toBe(3);
    for (const code of [
      'fleet_git_unsupported',
      'home_not_ready',
      'home_unverified',
      'slug_taken',
      'station_info_missing',
    ]) {
      expect(at('Some text of the deck.', code)).toBe(3);
    }
    expect(at('The session is already moving.')).toBe(4);
    expect(at('The session is being landed.')).toBe(4);
    expect(
      at('The session has not confirmed its conversation yet. Send it one prompt first.'),
    ).toBe(5);
    expect(at('Something new.')).toBeNull();
  });

  const target = (extra: Partial<MoveTarget> = {}): MoveTarget => ({
    station_id: 'station-b',
    name: 'Beta',
    kind: 'native',
    ready: false,
    ...extra,
  });

  test('items before the failed one passed, after it were not checked', () => {
    const list = checklist(
      target({ failed: { code: 'move_not_ready', text: 'The branch is not pushed.' } }),
    );
    expect(list.items.map((i) => i.mark)).toEqual([
      'passed',
      'passed',
      'failed',
      'neutral',
      'neutral',
      'neutral',
    ]);
    expect(list.items[2]?.text).toBe('The branch is not pushed.');
    expect(list.items.map((i) => i.label)).toEqual([...CHECKS]);
    expect(list.note).toBeNull();
  });

  test('a ready target passes all six; no target checks none', () => {
    expect(checklist(target({ ready: true })).items.every((i) => i.mark === 'passed')).toBe(true);
    const none = checklist(undefined);
    expect(none.items.every((i) => i.mark === 'neutral')).toBe(true);
    expect(none.note).toBeNull();
  });

  test('an unknown refusal leaves every item neutral and shows its text under the list', () => {
    const list = checklist(target({ failed: { code: 'new_code', text: 'A new reason.' } }));
    expect(list.items.every((i) => i.mark === 'neutral')).toBe(true);
    expect(list.note).toBe('A new reason.');
  });

  test('the kind line has the two texts of §12.8', () => {
    expect(kindLine('native')).toBe('The conversation moves with its history.');
    expect(kindLine('branch_only')).toBe(
      'Only the branch moves; a new session continues from the last summary.',
    );
  });
});

describe('the composer of a moved session', () => {
  const arrived = (state: MoveInState) =>
    summary('s2', 'working', 1, { move_in: { move_id: 'm1', state } });

  test('a source that moved and a held target have no composer', () => {
    const moved = summary('s1', 'working', 1, {
      moved_to: { move_id: 'm1', station_id: 'station-b', session_id: 's2' },
    });
    expect(composerHidden(moved)).toBe(true);
    for (const state of [
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
    ] as const) {
      expect(composerHidden(arrived(state))).toBe(true);
    }
  });

  test('a released or confirmed target and a plain session have one', () => {
    expect(composerHidden(arrived('released'))).toBe(false);
    expect(composerHidden(arrived('confirmed'))).toBe(false);
    expect(composerHidden(summary('s1', 'working', 1))).toBe(false);
  });

  test('the confirm line is for a released Claude session only', () => {
    expect(confirmLineShown(arrived('released'))).toBe(true);
    expect(confirmLineShown(arrived('confirmed'))).toBe(false);
    const codex = summary('s2', 'working', 1, {
      provider: 'codex',
      move_in: { move_id: 'm1', state: 'released' },
    });
    expect(confirmLineShown(codex)).toBe(false);
  });
});

describe('the lines of the banner', () => {
  const notice = (s: ReturnType<typeof summary>, m: MoveView | null, at = 'station-a') =>
    moveNotice({ station: at, summary: s, move: m, stations });

  const moved = summary('s1', 'working', 1, {
    moved_to: { move_id: 'm1', station_id: 'station-b', session_id: 's2' },
  });

  test('the source shows "Moved to" with an Open link to the new session', () => {
    const n = notice(moved, move('done'));
    expect(n?.text).toBe('Moved to Beta');
    expect(n?.action).toEqual({ kind: 'open', href: '#/s/station-b/s2' });
  });

  test('a resume_failed move gives the source the undo button', () => {
    const n = notice(moved, move('resume_failed'));
    expect(n?.text).toBe('Moved to Beta. The new session could not resume.');
    expect(n?.action).toEqual({ kind: 'undo', move: 'm1' });
  });

  test('undoing shows one line on both sides', () => {
    const target = summary('s2', 'working', 1, {
      move_in: { move_id: 'm1', state: 'failed' },
    });
    expect(notice(moved, move('undoing'))?.text).toBe('Undoing the move…');
    expect(notice(target, move('undoing'), 'station-b')?.text).toBe('Undoing the move…');
  });

  test('an open move shows its progress on the source; a failed one shows its text', () => {
    const plain = summary('s1', 'working', 1);
    expect(notice(plain, move('exporting', { bytes: 1_048_576 }))?.text).toBe(
      'Moving: copying the conversation (1.0 MiB)',
    );
    const failed = notice(plain, move('failed', { message: 'The move stopped.' }));
    expect(failed?.tone).toBe('hazard');
    expect(failed?.dismissible).toBe(true);
    expect(notice(plain, move('done'))).toBeNull();
    expect(notice(plain, null)).toBeNull();
  });

  const arrived = (state: MoveInState, text?: string) =>
    summary('s2', 'working', 1, {
      move_in: { move_id: 'm1', state, ...(text === undefined ? {} : { text }) },
    });

  test('the target card says where it came from while it arrives', () => {
    for (const state of ['staged', 'imported', 'launching', 'resumed'] as const) {
      expect(notice(arrived(state), move('starting'), 'station-b')?.text).toBe(
        'Arriving from Alpha…',
      );
    }
    expect(notice(arrived('staged'), null, 'station-b')?.text).toBe(
      'Arriving from the source station…',
    );
  });

  test('a target that could not resume has the text and the undo button of §12.4A', () => {
    const s = summary('s2', 'working', 1, {
      move_in: { move_id: 'm1', state: 'resume_failed', text: 'No conversation found' },
    });
    const n = notice(s, move('resume_failed'), 'station-b');
    expect(n?.text).toBe(
      'Claude could not resume the conversation on Beta: No conversation found. The conversation is still on Alpha, read only. Undo the move to continue there.',
    );
    expect(n?.action).toEqual({ kind: 'undo', move: 'm1' });
    const bare = notice(arrived('resume_failed'), move('resume_failed'), 'station-b');
    expect(bare?.text).toBe(
      'Claude could not resume the conversation on Beta. The conversation is still on Alpha, read only. Undo the move to continue there.',
    );
  });

  test('the ends of a move have the texts of §12.7', () => {
    const at = (state: MoveInState) => notice(arrived(state), move('done'), 'station-b')?.text;
    expect(at('failed')).toBe('This move was undone. The session continues on Alpha.');
    expect(at('abandoned')).toBe(
      'This copy of the conversation was not used. The session stays on Alpha.',
    );
    expect(at('discarded')).toBe('This move was cancelled.');
    expect(at('released')).toBeUndefined();
    expect(at('confirmed')).toBeUndefined();
  });

  test('a refused launch shows the text of the station, or nothing', () => {
    expect(notice(arrived('launch_refused', 'No room.'), null, 'station-b')?.text).toBe('No room.');
    expect(notice(arrived('launch_refused'), null, 'station-b')).toBeNull();
  });
});
