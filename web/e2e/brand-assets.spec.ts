// The board serves its icons and its manifest (the deck embeds dist/, which Vite fills from web/public).
import { expect, test } from '@playwright/test';
import { FakeDeck } from './fake-deck';

test('the board serves its icons and manifest', async ({ page }) => {
  await new FakeDeck().install(page);
  await page.goto('/');
  const iconHref = await page.locator('link[rel=icon][type="image/svg+xml"]').getAttribute('href');
  expect(iconHref).not.toBeNull();
  const icon = await page.request.get(new URL(iconHref ?? '', page.url()).toString());
  expect(icon.status()).toBe(200);
  expect(icon.headers()['content-type']).toContain('svg');

  const manifestHref = await page.locator('link[rel=manifest]').getAttribute('href');
  expect(manifestHref).not.toBeNull();
  const manifest = await page.request.get(new URL(manifestHref ?? '', page.url()).toString());
  expect(manifest.status()).toBe(200);
  expect(((await manifest.json()) as { name: string }).name).toBe('Fleet Deck');
});
