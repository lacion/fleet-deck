// M3 6-10 (DL-304, DL-305): image paste into prompts and needs-you
// notifications in a phone browser against the scripted deck.
import { expect, type Page, test } from '@playwright/test';
import type { DeckSession, OpenRequest } from '../src/gen';
import { summary } from '../src/testing/summary';
import { claudeCapabilities } from '../src/testing/timeline';
import { FakeDeck, fitsWidth, logIn, openFleet, openSession, stationView } from './fake-deck';

const ST = 'st-alpha';
// The 8-byte PNG signature; enough for the board, which does not decode it.
const PNG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function session(extra: Partial<DeckSession['summary']> = {}): DeckSession {
  return {
    station_id: ST,
    status: 'idle',
    summary: summary('s1', 'idle', 1, {
      title: 'Look at the bug',
      capabilities: claudeCapabilities(),
      ...extra,
    }),
  };
}

async function setUp(page: Page, deck: FakeDeck) {
  deck.stations = [stationView(ST, 'Alpha')];
  await deck.install(page);
  await logIn(page);
}

const idOf = (body: unknown) => (body as { command_id: string }).command_id;

test('a picked or pasted image goes with the prompt', async ({ page }) => {
  const deck = new FakeDeck();
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
  await setUp(page, deck);
  await openSession(page, 'Look at the bug');
  const composer = page.getByRole('form', { name: 'Composer' });
  await composer
    .getByLabel('Add image')
    .setInputFiles({ name: 'shot.png', mimeType: 'image/png', buffer: PNG });
  // A paste of a screenshot into the message box adds a second image.
  await composer.getByLabel('Message').evaluate((box, bytes) => {
    const data = new DataTransfer();
    data.items.add(new File([new Uint8Array(bytes)], 'paste.png', { type: 'image/png' }));
    box.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true }));
  }, Array.from(PNG));
  const images = composer.getByRole('list', { name: 'Images' });
  await expect(images.getByRole('img')).toHaveCount(2);
  await composer.getByRole('button', { name: 'Remove image 2' }).click();
  await expect(images.getByRole('img')).toHaveCount(1);
  await composer.getByLabel('Message').fill('what is wrong here?');
  expect(await fitsWidth(page)).toBe(true);
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  await expect(composer.getByRole('list', { name: 'Images' })).toHaveCount(0);

  const sent = deck.calls.filter((c) => c.path === '/api/commands/session.prompt');
  expect(sent.map((c) => (c.body as { command: unknown }).command)).toEqual([
    {
      op: 'prompt',
      session_id: 's1',
      input: {
        text: 'what is wrong here?',
        images: [{ media_type: 'image/png', data: PNG.toString('base64') }],
      },
    },
  ]);
});

test('a new needs-you request shows a browser notification once', async ({ page }) => {
  await page.addInitScript(() => {
    const shown: string[] = [];
    (window as unknown as { shown: string[] }).shown = shown;
    class FakeNotification {
      static permission = 'default';
      static requestPermission = async () => {
        FakeNotification.permission = 'granted';
        return 'granted';
      };
      onclick: (() => void) | null = null;
      constructor(title: string, options?: { body?: string }) {
        shown.push(`${title} | ${options?.body ?? ''}`);
      }
      close() {}
    }
    Object.defineProperty(window, 'Notification', { value: FakeNotification });
  });
  const request = (id: string, title: string): OpenRequest => ({
    request_id: id,
    node_id: null,
    request_kind: 'command',
    title,
    details: title,
    input: null,
    options: [{ option_id: 'allow', label: 'Allow' }],
    response: 'rpc_reply',
    requested_at: 1_760_000_000_000,
  });
  const deck = new FakeDeck();
  deck.sessions = [session({ open_requests: [request('r1', 'Bash: ls')] })];
  await setUp(page, deck);
  await openFleet(page);
  const rail = page.getByRole('region', { name: 'Needs you rail' });
  const bell = rail.getByRole('button', { name: 'Notifications' });
  await expect(bell).toHaveAttribute('aria-pressed', 'false');
  await bell.click();
  await expect(bell).toHaveAttribute('aria-pressed', 'true');

  deck.sessions = [
    session({ open_requests: [request('r1', 'Bash: ls'), request('r2', 'Bash: rm build')] }),
  ];
  deck.publish();
  await expect(rail).toContainText('Bash: rm build');
  deck.publish();
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { shown: string[] }).shown))
    .toEqual(['Needs you: Look at the bug | Bash: rm build']);
  expect(await fitsWidth(page)).toBe(true);
});
