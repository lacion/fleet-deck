// M3 6-8 (DL-097): browse and read the files of a session, show a turn diff
// from its checkpoints, revert a Codex turn and resolve a revert that needs
// reconciliation, at a phone viewport against the scripted deck.
import { expect, type Page, test } from '@playwright/test';
import type { DeckCommand, DeckSession, Event, SessionRevert } from '../src/gen';
import { codexCaps } from '../src/testing/capabilities';
import { summary } from '../src/testing/summary';
import { envelopes } from '../src/testing/timeline';
import { CSRF, FakeDeck, fitsWidth, logIn, openSession, stationView } from './fake-deck';

const ST = 'st-alpha';

function session(seq: number, revert: SessionRevert | null = null): DeckSession {
  return {
    station_id: ST,
    status: 'idle',
    summary: summary('s1', 'idle', 1_760_000_000_000 + seq * 1000, {
      provider: 'codex',
      title: 'Edit the docs',
      capabilities: codexCaps(),
      cwd: '/home/me/projects/app',
      session_seq: seq,
      revert,
    }),
  };
}

function accepted(command_id: string, method: string): DeckCommand {
  return {
    command_id,
    station_id: ST,
    method,
    origin: 'board',
    envelope: null,
    retry_until: 0,
    state: 'done',
    attempts: 1,
    sent_at: 1,
    receipt: { command_id, state: 'accepted', station_seq: 9, result: null, reject: null, at: 1 },
    reason: null,
    created_at: 1,
    updated_at: 1,
  };
}

const finalized = (turn: string): Event => ({
  kind: 'turn.finalized',
  turn_id: turn,
  checkpoint: {
    worktree_id: 'wt',
    scope: 'tracked',
    start: { state: 'available', commit: `${turn}-start` },
    end: { state: 'available', commit: `${turn}-end` },
  },
  diff_summary: { files_changed: 1, insertions: 1, deletions: 0 },
});

async function setUp(page: Page, deck: FakeDeck) {
  deck.stations = [stationView(ST, 'Alpha')];
  await deck.install(page);
  await logIn(page);
  deck.emit(ST, 's1', envelopes('s1', [finalized('t1'), finalized('t2')]));
  await openSession(page, 'Edit the docs');
  return page.getByRole('region', { name: 'Session' });
}

test('the files of a session are browsed, read and searched; a symlink is not opened', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.sessions = [session(1)];
  deck.on(`/api/stations/${ST}/files/list`, (_b, q) => {
    expect(q.get('session')).toBe('s1');
    const path = q.get('path') ?? '';
    const entries =
      path === ''
        ? [
            { name: 'docs', kind: 'dir', size: null },
            { name: 'a.txt', kind: 'file', size: 4 },
            { name: 'link', kind: 'symlink', size: null },
          ]
        : [{ name: 'guide.md', kind: 'file', size: 6 }];
    return { status: 200, body: { root: { session: 's1' }, path, entries, truncated: false } };
  });
  deck.on(`/api/stations/${ST}/files/read`, (_b, q) => {
    const path = q.get('path') ?? '';
    if (path === 'docs/guide.md') {
      return {
        status: 200,
        body: { path, size: 6, binary: false, text: '# Guide', truncated: false },
      };
    }
    return { status: 404, body: { code: 'not_found', message: 'no such file' } };
  });
  deck.on(`/api/stations/${ST}/files/search`, (_b, q) => ({
    status: 200,
    body: {
      hits: [{ path: 'docs/guide.md', line: 1, text: q.get('q') }],
      truncated: false,
      backend: q.get('mode') === 'content' ? 'git' : 'walk',
    },
  }));
  const view = await setUp(page, deck);
  await view.getByRole('tab', { name: 'Files' }).click();
  const files = view.getByRole('region', { name: 'Files' });
  // the root listing loads at once: there is no Browse button
  await expect(files.getByRole('button', { name: 'Browse' })).toHaveCount(0);
  await expect(files.locator('[data-entry-kind="symlink"]')).toContainText(
    'link (symlink, not opened)',
  );
  await expect(files.locator('[data-entry-kind="symlink"] button')).toHaveCount(0);
  // A folder row is named without a trailing "/" (the folder icon marks it).
  await files.getByRole('button', { name: 'docs', exact: true }).click();
  const crumbs = files.getByRole('navigation', { name: 'Folder path' });
  await expect(crumbs.getByRole('button', { name: 'docs' })).toBeVisible();
  await crumbs.getByRole('button', { name: 'root' }).click();
  await expect(files.getByRole('button', { name: /^a\.txt/ })).toBeVisible();
  await files.getByRole('button', { name: 'docs', exact: true }).click();
  // a file row names its size
  await files.getByRole('button', { name: /^guide\.md\s*6 B$/ }).click();
  // a Markdown file shows rendered; the Raw chip shows its text
  await expect(files.locator('.fd-files-md h1')).toHaveText('Guide');
  await expect(files.getByRole('button', { name: 'Rendered' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  await files.getByRole('button', { name: 'Raw' }).click();
  await expect(files.locator('.fd-file-text')).toHaveText('# Guide');
  await expect(files.locator('.fd-files-md')).toHaveCount(0);
  await files.getByRole('button', { name: 'Rendered' }).click();
  await expect(files.locator('.fd-files-md h1')).toHaveText('Guide');
  await files.getByRole('button', { name: 'Back to the folder' }).click();
  await files.getByRole('button', { name: '..' }).click();
  await files.getByRole('button', { name: /^a\.txt\s*4 B$/ }).click();
  await expect(files.getByRole('alert')).toHaveText('no such file');
  await files.getByLabel('Search', { exact: true }).fill('Guide');
  // Names | Content: two segments of one group (not a select).
  await files
    .getByRole('group', { name: 'Search in' })
    .getByRole('button', { name: 'Content' })
    .click();
  await files.getByRole('button', { name: 'Search', exact: true }).click();
  await expect(files.locator('[data-search="git"]')).toContainText('docs/guide.md:1');
  expect(await fitsWidth(page)).toBe(true);
});

test('a turn diff is shown, a revert is confirmed, and a reconciliation is resolved', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.sessions = [session(1)];
  deck.on(`/api/stations/${ST}/sessions/s1/turns/t2/diff`, () => ({
    status: 200,
    body: {
      session_id: 's1',
      turn_id: 't2',
      from_commit: 't2-start',
      to_commit: 't2-end',
      summary: { files_changed: 1, insertions: 1, deletions: 0 },
      patch: '+edited',
      truncated: false,
    },
  }));
  deck.on('/api/commands/session.revert', (body) => {
    const b = body as { command_id: string };
    deck.sessions = [
      session(2, {
        revert_id: 'rv-1',
        turn_id: 't2',
        state: 'reconciliation_required',
        reason: 'workspace_changed',
      }),
    ];
    queueMicrotask(() => deck.publish());
    return { status: 200, body: accepted(b.command_id, 'session.revert') };
  });
  deck.on('/api/commands/session.resolve_revert', (body) => {
    const b = body as { command_id: string };
    deck.sessions = [
      session(3, {
        revert_id: 'rv-1',
        turn_id: 't2',
        state: 'reconciled',
        reason: 'owner_reconciled',
      }),
    ];
    queueMicrotask(() => deck.publish());
    return { status: 200, body: accepted(b.command_id, 'session.resolve_revert') };
  });
  const view = await setUp(page, deck);
  await view.getByRole('tab', { name: /^Changes/ }).click();
  const turns = view.getByRole('region', { name: 'Turns' });
  const t2 = turns.locator('[data-turn="t2"]');
  await t2.getByRole('button', { name: 'Show the diff' }).click();
  await expect(t2.locator('[data-diff="t2"]')).toHaveText('+edited');
  await t2.getByRole('button', { name: 'Revert to here' }).click();
  const confirm = t2.getByRole('group', { name: 'Confirm the revert' });
  await confirm.getByRole('button', { name: 'Cancel' }).click();
  expect(deck.calls.some((c) => c.path === '/api/commands/session.revert')).toBe(false);
  await t2.getByRole('button', { name: 'Revert to here' }).click();
  await confirm.getByRole('button', { name: 'Revert', exact: true }).click();
  const revert = view.getByRole('region', { name: 'Revert' });
  await expect(revert.getByRole('alert')).toContainText('needs reconciliation');
  await expect(revert.getByRole('alert')).toContainText('Reason: workspace_changed.');
  await expect(t2.getByRole('button', { name: 'Revert to here' })).toBeDisabled();
  await revert.getByRole('button', { name: 'Mark as reconciled' }).click();
  await expect(revert).toHaveAttribute('data-revert-state', 'reconciled');
  await expect(revert.getByRole('button', { name: 'Mark as reconciled' })).toHaveCount(0);
  const posted = deck.calls.filter((c) => c.path.startsWith('/api/commands/'));
  expect(posted.map((c) => c.csrf)).toEqual([CSRF, CSRF]);
  expect(posted.map((c) => (c.body as { command: unknown }).command)).toEqual([
    { op: 'revert', session_id: 's1', turn_id: 't2' },
    { op: 'resolve_revert', session_id: 's1', revert_id: 'rv-1', resolution: 'reconciled' },
  ]);
  expect(await fitsWidth(page)).toBe(true);
});

test('a file name of a diff opens the file in the Files tab', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [session(1)];
  deck.on(`/api/stations/${ST}/files/list`, (_b, q) => ({
    status: 200,
    body: { root: { session: 's1' }, path: q.get('path') ?? '', entries: [], truncated: false },
  }));
  deck.on(`/api/stations/${ST}/sessions/s1/turns/t2/diff`, () => ({
    status: 200,
    body: {
      session_id: 's1',
      turn_id: 't2',
      from_commit: 't2-start',
      to_commit: 't2-end',
      summary: { files_changed: 1, insertions: 1, deletions: 0 },
      patch: 'diff --git a/src/a.txt b/src/a.txt\n+edited',
      truncated: false,
    },
  }));
  const reads: string[] = [];
  deck.on(`/api/stations/${ST}/files/read`, (_b, q) => {
    const path = q.get('path') ?? '';
    reads.push(path);
    return {
      status: 200,
      body: { path, size: 6, binary: false, text: 'edited', truncated: false },
    };
  });
  const view = await setUp(page, deck);
  await view.getByRole('tab', { name: /^Changes/ }).click();
  const t2 = view.getByRole('region', { name: 'Turns' }).locator('[data-turn="t2"]');
  await t2.getByRole('button', { name: 'Show the diff' }).click();
  await t2.locator('[data-diff="t2"]').getByRole('button', { name: 'src/a.txt' }).click();
  await expect(view.getByRole('tab', { name: 'Files' })).toHaveAttribute('aria-selected', 'true');
  const files = view.getByRole('region', { name: 'Files' });
  await expect(files.locator('[data-file="src/a.txt"] .fd-file-text')).toHaveText('edited');
  await expect(files.getByRole('navigation', { name: 'Folder path' })).toContainText('src');
  // the root listing did not replace the file that the request opened
  expect(reads).toEqual(['src/a.txt']);
  expect(deck.calls.filter((c) => c.path === `/api/stations/${ST}/files/list`)).toHaveLength(0);
  expect(await fitsWidth(page)).toBe(true);
});

test('the turn diff button loads, shows and hides the diff of its turn', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [session(1)];
  const path = `/api/stations/${ST}/sessions/s1/turns/t2/diff`;
  deck.on(path, () => ({
    status: 200,
    body: {
      session_id: 's1',
      turn_id: 't2',
      from_commit: 't2-start',
      to_commit: 't2-end',
      summary: { files_changed: 1, insertions: 1, deletions: 0 },
      patch: '+edited',
      truncated: false,
    },
  }));
  // The first load waits until the test releases it: the button shows its loading state.
  const release = deck.hold(path);
  const view = await setUp(page, deck);
  await view.getByRole('tab', { name: /^Changes/ }).click();
  const t2 = view.getByRole('region', { name: 'Turns' }).locator('[data-turn="t2"]');
  const button = t2.locator('button.fd-turn-diff');
  await expect(button).toHaveText('Show the diff');
  await expect(button).toHaveAttribute('aria-expanded', 'false');
  await button.click();
  await expect(button).toHaveText('Loading…');
  await expect(button).toHaveAttribute('aria-busy', 'true');
  await expect(t2.locator('[data-diff="t2"]')).toHaveCount(0);
  release();
  await expect(button).toHaveText('Hide the diff');
  await expect(button).toHaveAttribute('aria-expanded', 'true');
  await expect(button).not.toHaveAttribute('aria-busy');
  await expect(t2.locator('[data-diff="t2"]')).toHaveText('+edited');
  // A second press hides the diff.
  await button.click();
  await expect(button).toHaveText('Show the diff');
  await expect(button).toHaveAttribute('aria-expanded', 'false');
  await expect(t2.locator('[data-diff="t2"]')).toHaveCount(0);
});
