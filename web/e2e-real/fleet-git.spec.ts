// FG-1, FG-2, FG-8 (design §19, T4.5b): the Fleet Git flows on a phone against real processes: one
// deck, one station `alpha` and one bare repository `remote.git`. The steps share one fleet and
// one phone page, in order: the project Git settings save and the home is verified, a sync start
// gets the branch `fd/alpha/<callsign>`, a turn pushes the branch (read on the station and in the
// bare repository), and a merge land ends "Landed". There is no move case here (T4.6b).
import { writeFileSync } from 'node:fs';
import { expect, type Locator, type Page, test } from '@playwright/test';
import { Phone } from './board';
import { git, GitRig, waitFor } from './fleet';

test.describe.configure({ mode: 'serial' });

let rig: GitRig;
let phone: Phone;
let page: Page;
let projectId: string;
let sessionId: string;
let branch: string;
let commit: string;

test.beforeAll(async ({ browser }) => {
  rig = await GitRig.create();
  // The project exists before the board logs in, so its first project list has it.
  projectId = await rig.createProject('App');
  phone = await Phone.open(browser, rig.deck);
  page = phone.page;
  await page.goto(rig.deck.loginLink());
  await phone.live();
});

test.afterAll(async () => {
  await phone?.context.close();
  await rig?.dispose();
});

/** The session screen of `sessionId`, opened from the fleet list. */
async function openSession(): Promise<Locator> {
  await phone.fleet();
  await phone.card(sessionId).click();
  const view = page.getByRole('region', { name: 'Session', exact: true });
  await expect(view).toBeVisible();
  return view;
}

test('the project Git settings save in the board and the station verifies the home', async () => {
  await page.evaluate(() => {
    window.location.hash = '#/projects';
  });
  // The row actions are in one row menu (the audit's Projects table).
  const row = page.getByRole('row', { name: 'Project App' });
  await row.locator('summary[aria-label="Actions for App"]').click();
  await row.getByRole('button', { name: 'Edit' }).click();
  const form = page.getByRole('form', { name: 'Git sync' });
  await expect(form).toBeVisible();
  await form.getByLabel('Sync this project across stations').check();
  // Fleet Sync: the remote URL is in the closed "Advanced" part.
  await form.getByText('Advanced', { exact: true }).click();
  await form.getByLabel('Remote URL', { exact: true }).fill(rig.remoteUrl);
  await form.getByLabel('Push branches').selectOption('on_turn');
  expect(await phone.fitsWidth()).toBe(true);
  await form.getByRole('button', { name: 'Save Git settings' }).click();
  await expect(form.getByText('Saved.')).toBeVisible();

  const home = await waitFor('the home is ready', async () => {
    const project = await rig.project(projectId);
    return project.homes?.[0]?.state === 'ready' ? project : null;
  });
  expect(home.git).toMatchObject({ sync: true, push: 'on_turn' });
  expect(home.remote_url).toBe(rig.remoteUrl);
});

test('a sync start gets the branch fd/alpha/<callsign>', async () => {
  sessionId = await rig.startWorktree();
  const first = await rig.firstTurn(sessionId);
  expect(first.callsign).toMatch(/^[a-z0-9-]+$/);
  expect(first.branch).toBe(`fd/alpha/${first.callsign}`);
  expect(first.worktree).not.toBeNull();
  branch = first.branch ?? '';
  const view = await openSession();
  await expect(view).toContainText(branch);
  expect(await phone.fitsWidth()).toBe(true);
});

test('after a turn the station pushes the branch to the bare repository', async () => {
  const worktree = (await rig.firstTurn(sessionId)).worktree ?? '';
  commit = commitIn(worktree, 'feature.txt', 'feature\n');
  const view = await openSession();
  const composer = page.getByRole('form', { name: 'Composer' });
  await composer.getByLabel('Message').fill('push this work');
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(view.getByRole('log')).toContainText('echo: push this work');

  // The push is read on the station and in the bare repository, not in the board.
  const pushed = await rig.waitSession(
    'the push',
    sessionId,
    (s) => s.push?.state === 'pushed' && s.push.oid === commit,
  );
  expect(pushed.push?.ref_name).toBe(branch);
  expect(git(rig.remote, ['rev-parse', `refs/heads/${branch}`])).toBe(commit);
});

test('a merge land ends Landed and advances main', async () => {
  const view = await openSession();
  const land = view.getByRole('region', { name: 'Land', exact: true });
  await land.getByRole('button', { name: 'Land', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: `Land ${branch}?` });
  await expect(dialog).toBeVisible();
  expect(await phone.fitsWidth()).toBe(true);
  await dialog.getByRole('button', { name: 'Land', exact: true }).click();
  await expect(land.getByText(/^Landed/)).toBeVisible();
  expect(git(rig.remote, ['show', 'main:feature.txt'])).toBe('feature');
});

/** Commits `file` in the worktree; returns the commit id. */
function commitIn(dir: string, file: string, content: string): string {
  writeFileSync(`${dir}/${file}`, content);
  git(dir, ['add', '--', file]);
  git(dir, ['commit', '-q', '-m', file]);
  return git(dir, ['rev-parse', 'HEAD']);
}
