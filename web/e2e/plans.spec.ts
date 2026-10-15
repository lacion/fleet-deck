// M3 6-6 (DL-301): the plan library at a phone viewport against the
// scripted deck: read a captured plan, archive and restore it, and start a
// new session with the plan as its first prompt.
import { expect, test } from '@playwright/test';
import type { DeckPlan } from '../src/gen';
import { claudeCaps } from '../src/testing/capabilities';
import { FakeDeck, fitsWidth, logIn, openMore, stationView } from './fake-deck';
import { threeSessionDeck } from './scenes';

const ST = 'st-alpha';

function plan(id: string, status: DeckPlan['status'], at: number): DeckPlan {
  return {
    id: `${ST}/s1/${id}`,
    station_id: ST,
    session_id: 's1',
    plan_id: id,
    turn_id: 't1',
    session_title: 'Plan the cache',
    markdown: `# Cache plan ${id}\n\n1. Read store.rs\n2. Add an LRU cache`,
    truncated: false,
    status,
    proposed_at: 1_760_000_000_000 + at,
    updated_at: 1_760_000_000_000 + at,
  };
}

test('a captured plan is read, archived, restored and starts a session', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [
    stationView(ST, 'Alpha', {
      info: {
        name: 'alpha',
        incarnation: 'inc-1',
        head: 3,
        live_sessions: 0,
        version: '0.2.0',
        max_sessions: 4,
        allow_shell: null,
        providers: [
          {
            instance: 'personal',
            provider: 'claude',
            version: '2.1.5',
            auth: 'signed_in',
            version_status: 'unknown',
            capabilities: claudeCaps(),
          },
        ],
      },
      info_at: 1_760_000_000_000,
    }),
  ];
  let plans = [plan('p2', 'proposed', 2), plan('p1', 'proposed', 1)];
  deck.on('/api/plans', (_body, query) => ({
    status: 200,
    body: query.get('archived') === 'true' ? plans : plans.filter((p) => p.status !== 'archived'),
  }));
  deck.on('/api/plans/mark', (body) => {
    const b = body as { id: string; status: DeckPlan['status'] };
    plans = plans.map((p) => (p.id === b.id ? { ...p, status: b.status } : p));
    const row = plans.find((p) => p.id === b.id);
    return row === undefined
      ? { status: 404, body: { code: 'unknown_plan', message: 'No such plan.' } }
      : { status: 200, body: row };
  });
  await deck.install(page);
  await logIn(page);

  await openMore(page, 'Plans');
  const library = page.getByRole('region', { name: 'Plan library' });
  const p1 = library.getByRole('article', { name: 'Plan Cache plan p1' });
  await expect(library.getByRole('article')).toHaveCount(2);
  await expect(p1).toContainText('Proposed');
  await expect(p1).toContainText('Alpha');
  await p1.getByText('Show plan').click();
  await expect(p1.locator('pre')).toContainText('2. Add an LRU cache');
  expect(await fitsWidth(page)).toBe(true);

  await p1.getByRole('button', { name: 'Archive' }).click();
  await expect(library.getByRole('article')).toHaveCount(1);
  await library.getByLabel('Show archived').check();
  await expect(p1).toContainText('Archived');
  await p1.getByRole('button', { name: 'Restore' }).click();
  await expect(p1).toContainText('Proposed');
  const marks = deck.calls.filter((c) => c.path === '/api/plans/mark').map((c) => c.body);
  expect(marks).toEqual([
    { id: `${ST}/s1/p1`, status: 'archived' },
    { id: `${ST}/s1/p1`, status: 'proposed' },
  ]);

  await p1.getByRole('button', { name: 'Start a session with this plan' }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await expect(form.getByLabel('What should the agent do? (optional)')).toHaveValue(
    '# Cache plan p1\n\n1. Read store.rs\n2. Add an LRU cache',
  );
});

test('without drive scope the plan actions are disabled', async ({ page }) => {
  const deck = new FakeDeck();
  deck.scopes = ['read'];
  deck.on('/api/plans', () => ({ status: 200, body: [plan('p1', 'proposed', 1)] }));
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Plans');
  const p1 = page.getByRole('article', { name: 'Plan Cache plan p1' });
  await expect(p1.getByRole('button', { name: 'Archive' })).toBeDisabled();
  await expect(p1.getByRole('button', { name: 'Start a session with this plan' })).toBeDisabled();
});

test('a plan is assigned to a live session as mail, then marked executed', async ({ page }) => {
  const deck = threeSessionDeck();
  let plans = [plan('p1', 'proposed', 1)];
  deck.on('/api/plans', () => ({ status: 200, body: plans }));
  deck.on('/api/plans/mark', (body) => {
    const b = body as { id: string; status: DeckPlan['status'] };
    plans = plans.map((p) => (p.id === b.id ? { ...p, status: b.status } : p));
    return { status: 200, body: plans[0] };
  });
  deck.on('/api/mail', () => ({ status: 200, body: {} }));
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Plans');
  const p1 = page.getByRole('article', { name: 'Plan Cache plan p1' });
  await p1.getByRole('button', { name: 'Assign to a session…' }).click();
  const form = p1.locator('form');
  await expect(form.getByLabel('Session')).toHaveValue(`${ST}/s1`);
  await form.getByLabel('Session').selectOption(`${ST}/s2`);
  await form.getByLabel('Instructions (optional)').fill('Start with the tests.');
  await form.getByRole('button', { name: 'Send as mail' }).click();
  await expect(p1.getByRole('status')).toHaveText('Sent to Write the docs.');
  await expect(p1).toContainText('Executed');

  const mails = deck.calls.filter((c) => c.path === '/api/mail');
  expect(mails).toHaveLength(1);
  expect(mails[0]?.body).toMatchObject({
    station: ST,
    session_id: 's2',
    delivery: 'follow_up',
    text: `A plan from the library for you to execute.\n\n${plan('p1', 'proposed', 1).markdown}\n\nInstructions: Start with the tests.`,
  });
  const marks = deck.calls.filter((c) => c.path === '/api/plans/mark').map((c) => c.body);
  expect(marks).toEqual([{ id: `${ST}/s1/p1`, status: 'executed' }]);
});

test('a refused mail keeps the form open with the reason', async ({ page }) => {
  const deck = threeSessionDeck();
  deck.on('/api/plans', () => ({ status: 200, body: [plan('p1', 'proposed', 1)] }));
  deck.on('/api/mail', () => ({
    status: 409,
    body: { code: 'session_ended', message: 'The session ended.' },
  }));
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Plans');
  const p1 = page.getByRole('article', { name: 'Plan Cache plan p1' });
  await p1.getByRole('button', { name: 'Assign to a session…' }).click();
  await p1.getByRole('button', { name: 'Send as mail' }).click();
  await expect(p1.getByRole('alert')).toContainText('The session ended.');
  await expect(p1.locator('form')).toBeVisible();
  expect(deck.calls.some((c) => c.path === '/api/plans/mark')).toBe(false);
  await p1.getByRole('button', { name: 'Cancel' }).click();
  await expect(p1.locator('form')).toHaveCount(0);
});
