// The mark on the login page draws in the text color: the board theme, and forced colors.
import { expect, test } from '@playwright/test';
import { FakeDeck } from './fake-deck';

const MODES = [
  { name: 'light', media: { colorScheme: 'light' } },
  { name: 'dark', media: { colorScheme: 'dark' } },
  { name: 'forced colors', media: { forcedColors: 'active', colorScheme: 'light' } },
] as const;

test('the login mark draws in the text color in light, dark and forced colors', async ({
  page,
}) => {
  await new FakeDeck().install(page);
  await page.goto('/');
  await expect(page.locator('.fd-login .fd-mark path')).toHaveCount(1);
  for (const mode of MODES) {
    await page.emulateMedia(mode.media);
    // One read of both colors: the theme changes on the next frames, so poll until they settle.
    await expect
      .poll(
        () =>
          page.evaluate(() => {
            const path = document.querySelector('.fd-login .fd-mark path');
            const title = document.querySelector('.fd-login h1');
            return (
              path !== null &&
              title !== null &&
              getComputedStyle(path).fill === getComputedStyle(title).color
            );
          }),
        { message: mode.name },
      )
      .toBe(true);
  }
});
