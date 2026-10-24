// M3 6-13 (PLAN §16): v0 parity rows that had no board flow yet, in a phone
// browser against the scripted deck.
import { expect, test } from '@playwright/test';
import type { DeckSession } from '../src/gen';
import { summary } from '../src/testing/summary';
import { claudeCapabilities } from '../src/testing/timeline';
import { FakeDeck, logIn, openSession, stationView } from './fake-deck';

const ST = 'st-alpha';

function session(): DeckSession {
  return {
    station_id: ST,
    status: 'idle',
    summary: summary('s1', 'idle', 1, {
      title: 'Write the notes',
      capabilities: claudeCapabilities(),
    }),
  };
}

const idOf = (body: unknown) => (body as { command_id: string }).command_id;

test('Shift+Enter in the composer adds a newline and sends nothing (v0 parity)', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [session()];
  deck.on('/api/commands/session.prompt', (body) => ({
    status: 200,
    body: {
      command_id: idOf(body),
      station_id: ST,
      method: 'session.prompt',
      origin: 'board',
      envelope: null,
      retry_until: 0,
      state: 'done',
      receipt: { state: 'accepted', reject: null },
      reason: null,
    },
  }));
  await deck.install(page);
  await logIn(page);
  await openSession(page, 'Write the notes');
  const composer = page.getByRole('form', { name: 'Composer' });
  const box = composer.getByLabel('Message');
  await box.click();
  await page.keyboard.type('first line');
  await page.keyboard.press('Shift+Enter');
  await page.keyboard.type('second line');
  await expect(box).toHaveValue('first line\nsecond line');
  expect(deck.calls.filter((c) => c.path === '/api/commands/session.prompt')).toHaveLength(0);

  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  const sent = deck.calls.filter((c) => c.path === '/api/commands/session.prompt');
  expect(sent.map((c) => (c.body as { command: unknown }).command)).toEqual([
    { op: 'prompt', session_id: 's1', input: { text: 'first line\nsecond line' } },
  ]);
});
