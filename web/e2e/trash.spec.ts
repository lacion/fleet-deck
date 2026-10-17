// DL-150: the trash buttons of a worktree on the stations screen.
import { expect, test } from '@playwright/test';
import type { WorktreeInfo } from '../src/gen';
import { FakeDeck, logIn, openMore, stationView } from './fake-deck';

const worktree = (extra: Partial<WorktreeInfo> = {}): WorktreeInfo => ({
  session_id: 's1',
  title: 'Fix the login',
  path: '/w/deck-wt/s1',
  branch: 'fd/s1',
  main: '/w/deck',
  exists: true,
  registered: true,
  dirty: 0,
  ignored: 0,
  unpushed: 0,
  compared_with: 'remotes',
  last_commit: null,
  live: false,
  verdict: 'clean',
  note: null,
  ...extra,
});

const accepted = { status: 200, body: { state: 'done', receipt: { state: 'accepted' } } };

test('a clean worktree moves to the trash, then is deleted with the typed folder name', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.stations = [stationView('st-alpha', 'Alpha')];
  let listed: WorktreeInfo = worktree();
  deck.on('/api/stations/Alpha/worktrees', () => ({ status: 200, body: { worktrees: [listed] } }));
  deck.on('/api/stations/Alpha/worktrees/trash', () => {
    listed = worktree({
      verdict: 'trashed',
      trash: {
        state: 'trashed',
        phase: null,
        message: null,
        trash_path: '/w/.trash/s1-abc',
        trashed_at: Date.now() - 3.5 * 3_600_000,
      },
    });
    return accepted;
  });
  deck.on('/api/stations/Alpha/worktrees/purge', () => accepted);
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Stations');
  const section = page.locator('[data-station="st-alpha"] details.fd-wt');
  await section.locator('summary').click();
  const row = section.locator('li.fd-wt-row');
  await expect(row).toHaveAttribute('data-verdict', 'clean');

  page.once('dialog', (dialog) => {
    expect(dialog.message()).toBe('Move the worktree s1 to the trash? It can be restored.');
    void dialog.accept();
  });
  await row.getByRole('button', { name: 'Move to trash' }).click();
  await expect(row).toHaveAttribute('data-verdict', 'trashed');
  await expect(row).toContainText('In the trash since 3h ago');
  await expect(row).toContainText('/w/.trash/s1-abc');
  const trashCall = deck.calls.find((c) => c.path.endsWith('/worktrees/trash'));
  expect(trashCall?.body).toMatchObject({ session_id: 's1' });
  expect(trashCall?.body).not.toHaveProperty('confirm_name');

  await row.getByRole('button', { name: 'Delete permanently' }).click();
  const input = row.getByLabel('Type the folder name to confirm');
  const confirm = row.getByRole('button', { name: 'Delete', exact: true });
  await expect(confirm).toBeDisabled();
  await input.fill('s1-ab');
  await expect(confirm).toBeDisabled();
  await input.fill('s1-abc');
  await expect(confirm).toBeEnabled();
  await confirm.click();
  await expect(row.getByRole('status')).toHaveText('Deleting the trashed folder…');
  const purgeCall = deck.calls.find((c) => c.path.endsWith('/worktrees/purge'));
  expect(purgeCall?.body).toMatchObject({ session_id: 's1', confirm_name: 's1-abc' });
});
