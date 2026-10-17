// FG-1, FG-2 (design §4.4, §5.9, §5.10): the card chips of Fleet Git and the timeline lines.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, PushInfo } from '../gen';
import { integrationActions } from '../integrations/actions';
import { IntegrationsProvider } from '../integrations/context';
import { fakeActions } from '../integrations/fixtures';
import { ApiError } from '../runtime/api';
import { buildTimeline, type Item } from '../session/timeline';
import { keptText } from '../session/timeline_fleet_git';
import { card } from '../testing/fleet';
import { envelopes } from '../testing/timeline';
import { CardNote } from './cardParts';
import { PUSHING_AFTER_MS, PushBadge, pushBadgeOf } from './PushBadge';

const NOW = 1_760_000_000_000;
const LONG = 'b'.repeat(40);

const push = (extra: Partial<PushInfo> = {}): PushInfo => ({
  state: 'failing',
  ref_name: 'fd/alpha/ada',
  attempts: 3,
  ...extra,
});

const withPush = (p: PushInfo | undefined, more: Partial<DeckSession['summary']> = {}) =>
  card('s1', 'idle', 'station-a', { ...(p === undefined ? {} : { push: p }), ...more });

const badge = (c: DeckSession, extra: { dueSince?: number } = {}, canDrive = true) =>
  renderToStaticMarkup(
    <IntegrationsProvider actions={fakeActions({ canDrive })} initial={[]}>
      <PushBadge card={c} now={NOW} {...extra} />
    </IntegrationsProvider>,
  );

describe('the push badge', () => {
  test('failing shows "Not pushed" with its title and the Push now button', () => {
    const html = badge(
      withPush(push({ text: 'Push was rejected: stale info', next_attempt_at: NOW + 4 * 60_000 })),
    );
    expect(html).toContain('>Not pushed</span>');
    expect(html).toContain(
      'title="Push failed 3 times: Push was rejected: stale info. Next try in 4 min."',
    );
    expect(html).toContain('>Push now</button>');
  });

  test('the title has no "Next try" without a next attempt', () => {
    const html = badge(withPush(push({ text: 'x' })));
    expect(html).toContain('title="Push failed 3 times: x."');
    expect(html).not.toContain('Next try');
  });

  test('without a drive scope there is no Push now button', () => {
    const html = badge(withPush(push()), {}, false);
    expect(html).toContain('Not pushed');
    expect(html).not.toContain('Push now');
  });

  test('due or pushing shows "Pushing…" only after 60 seconds', () => {
    for (const state of ['due', 'pushing'] as const) {
      const c = withPush(push({ state }));
      expect(badge(c, { dueSince: NOW - 30_000 })).toBe('');
      expect(badge(c, { dueSince: NOW - PUSHING_AFTER_MS })).toBe('');
      expect(badge(c, { dueSince: NOW - PUSHING_AFTER_MS - 1 })).toContain('>Pushing…</span>');
    }
    // The first render of a board has not seen the push before: nothing yet.
    expect(badge(withPush(push({ state: 'pushing' })))).toBe('');
  });

  test('pushed, idle and no push show nothing', () => {
    expect(badge(withPush(push({ state: 'pushed', oid: 'abcdef123456' })))).toBe('');
    expect(badge(withPush(push({ state: 'idle' })))).toBe('');
    expect(badge(withPush(undefined))).toBe('');
  });

  test('"Push stopped" needs an unpublished commit and gives the title by stop_code', () => {
    const stopped = (extra: Partial<PushInfo>) =>
      pushBadgeOf(push({ state: 'stopped', ...extra }), NOW, null);
    expect(
      stopped({ stop_code: 'land_published', desired_oid: LONG, oid: 'a'.repeat(40) }),
    ).toEqual({
      kind: 'stopped',
      label: 'Push stopped',
      title:
        'The land pushed a rebased branch. New commits on this branch are not pushed automatically.',
    });
    expect(
      stopped({ stop_code: 'project_gone', desired_oid: LONG, oid: 'abcdef1234567890' })?.title,
    ).toBe('Sync is off for this project. Commits after abcdef1 are not pushed.');
    expect(stopped({ stop_code: 'project_gone', desired_oid: LONG })?.title).toBe(
      'Sync is off for this project. Commits after the start are not pushed.',
    );
    expect(stopped({ stop_code: 'unsupported_mode', desired_oid: LONG })?.title).toBe(
      'This station does not know the push mode of the project. Update the station.',
    );
    // No unpublished commit, a moved row and a move_failed row show nothing.
    expect(stopped({ stop_code: 'project_gone' })).toBeNull();
    expect(stopped({ stop_code: 'project_gone', desired_oid: 'a', oid: 'a' })).toBeNull();
    expect(stopped({ stop_code: 'moved', desired_oid: 'b' })).toBeNull();
    expect(stopped({ stop_code: 'move_failed', desired_oid: 'b' })).toBeNull();
  });

  test('the card shows "Stale base" and the push badge before the note', () => {
    const c = withPush(push(), { base_stale: true });
    const html = renderToStaticMarkup(<CardNote card={c} />);
    expect(html).toContain('data-chip="stale-base"');
    expect(html).toContain('>Stale base</span>');
    expect(html).toContain('>Not pushed</span>');
    // A card of a project with sync off shows nothing new.
    expect(renderToStaticMarkup(<CardNote card={card('p')} />)).not.toContain('fd-card-gitchips');
  });
});

describe('the Push now call', () => {
  const rig = (fail?: ApiError) => {
    const calls: [string, unknown][] = [];
    const io = {
      get: async () => ({}),
      put: async () => ({}),
      del: async () => null,
      post: async (path: string, body: unknown) => {
        calls.push([path, body]);
        if (fail !== undefined) throw fail;
        return { state: 'due' };
      },
    };
    return { calls, actions: integrationActions(io, ['read', 'drive']) };
  };

  test('it posts to the push route of the session', async () => {
    const { calls, actions } = rig();
    expect(await actions.pushNow('st a', 'ses/1')).toEqual({ state: 'due' });
    expect(calls).toEqual([['/api/sessions/st%20a/ses%2F1/push', {}]]);
  });

  test('the three named errors get the texts of the design; others keep the deck message', async () => {
    const text = (status: number, code: string, message = 'deck text') =>
      rig(new ApiError(status, message, code))
        .actions.pushNow('s', 'x')
        .catch((e: Error) => e.message);
    expect(await text(409, 'not_synced')).toBe('This session is not in a synced project.');
    expect(await text(409, 'held')).toBe('A land or a move holds this session.');
    expect(await text(404, 'unknown_session')).toBe('The station does not know this session.');
    expect(await text(504, 'timeout', 'The station did not answer.')).toBe(
      'The station did not answer.',
    );
  });
});

const notices = (events: Parameters<typeof envelopes>[1]): string[] =>
  buildTimeline(envelopes('s1', events))
    .items.filter((i): i is Extract<Item, { type: 'notice' }> => i.type === 'notice')
    .map((i) => `${i.tone}: ${i.text}`);

describe('the timeline lines of the Fleet Git events', () => {
  test('base_stale is a notice with the event text', () => {
    expect(
      notices([
        {
          kind: 'workspace.base_stale',
          code: 'fetch_timeout',
          text: 'Fetch took more than 30 s; this session started from the local HEAD.',
        },
      ]),
    ).toEqual(['info: Fetch took more than 30 s; this session started from the local HEAD.']);
  });

  test('include_skipped is one notice with one line for each file and the reason texts', () => {
    const reasons = [
      ['symlink', 'a link'],
      ['not_regular', 'not a regular file'],
      ['too_large', 'larger than 1 MiB'],
      ['total_too_large', 'over the 10 MiB total'],
      ['exists', 'already in the worktree'],
      ['unreadable', 'not readable'],
      ['not_ignored', 'not ignored by Git'],
      ['too_many_matches', 'more than 200 matches'],
      ['future_reason', 'future_reason'],
    ] as const;
    const out = notices([
      {
        kind: 'workspace.include_skipped',
        files: reasons.map(([reason], i) => ({ path: `f${i}`, reason })),
      },
    ]);
    expect(out).toHaveLength(1);
    expect(out[0]).toBe(
      `info: ${reasons.map(([, text], i) => `Not copied: f${i} (${text})`).join('\n')}`,
    );
  });

  test('include_skipped lists at most 50 files and an empty list has no line', () => {
    const files = Array.from({ length: 60 }, (_, i) => ({ path: `p${i}`, reason: 'exists' }));
    const out = notices([{ kind: 'workspace.include_skipped', files }]);
    expect(out[0]?.replace('info: ', '').split('\n')).toHaveLength(50);
    expect(notices([{ kind: 'workspace.include_skipped', files: [] }])).toEqual([]);
  });

  const pushEvent = (state: PushInfo['state'], extra: Partial<PushInfo> = {}) =>
    ({ kind: 'session.push', state, ref_name: 'fd/a/b', attempts: 1, ...extra }) as const;

  test('failing is an error line with the text of the station', () => {
    expect(
      notices([
        pushEvent('failing', { code: 'push_rejected', text: 'Push was rejected: stale info' }),
      ]),
    ).toEqual(['error: Push was rejected: stale info']);
    expect(notices([pushEvent('failing', { code: 'push_timeout' })])).toEqual([
      'error: Push took more than 60 s.',
    ]);
  });

  test('stopped is an error line', () => {
    expect(notices([pushEvent('stopped', { stop_code: 'land_published' })])).toEqual([
      'error: Push stopped: The land pushed a rebased branch. New commits on this branch are not pushed automatically.',
    ]);
    expect(notices([pushEvent('stopped', { stop_code: 'unsupported_mode', text: 'x' })])).toEqual([
      'error: Push stopped: x',
    ]);
  });

  test('pushed gets a line only after failing or stopped; the other states have none', () => {
    const oid = 'abcdef1234567890';
    expect(
      notices([
        pushEvent('due'),
        pushEvent('pushing'),
        pushEvent('pushed', { oid }),
        pushEvent('idle'),
      ]),
    ).toEqual([]);
    expect(
      notices([
        pushEvent('failing', { text: 'Push failed: x' }),
        pushEvent('pushed', { oid }),
        pushEvent('pushed', { oid }),
      ]),
    ).toEqual(['error: Push failed: x', 'info: Pushed abcdef1']);
    expect(
      notices([pushEvent('stopped', { stop_code: 'moved' }), pushEvent('pushed', { oid })]),
    ).toEqual(['error: Push stopped: moved', 'info: Pushed abcdef1']);
  });

  test('cleanup: trashed and a changed kept reason give lines; waiting and repeats do not', () => {
    const cleanup = (state: 'waiting' | 'kept' | 'trashed', reason?: string) =>
      ({ kind: 'session.cleanup', state, ...(reason === undefined ? {} : { reason }) }) as const;
    expect(
      notices([
        cleanup('waiting'),
        cleanup('kept', 'dirty'),
        cleanup('kept', 'dirty'),
        cleanup('kept', 'not_pushed'),
        cleanup('kept', 'dirty'),
        cleanup('trashed'),
      ]),
    ).toEqual([
      'info: Kept: the worktree has changes or new files.',
      'info: Kept: the last commit is not pushed.',
      'info: Kept: the worktree has changes or new files.',
      'info: Moved to the trash by cleanup. Restore it from Stations → Worktrees.',
    ]);
  });

  test('the kept texts drop the clause of a value that the event does not carry', () => {
    expect(keptText('held')).toBe('Kept: a land or a move holds this session.');
    expect(keptText('detached')).toBe('Kept: the worktree is not on a branch (detached HEAD).');
    expect(keptText('branch_changed')).toBe('Kept: the worktree is on another branch.');
    expect(keptText('changed')).toBe('Kept: the worktree changed after the cleanup checks.');
    expect(keptText('fetch_failed')).toBe('Kept: the fetch failed; cleanup tries again later.');
    expect(keptText('not_merged')).toBe(
      'Kept: the branch is not merged. A squash merge is not detected; trash the worktree yourself.',
    );
    expect(keptText('trash_refused')).toBe('Kept.');
    expect(keptText('trash_refused: the session is open')).toBe('Kept: the session is open');
    expect(keptText('other')).toBe('Kept: other');
  });
});
