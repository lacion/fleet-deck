// Every popover menu (`details.fd-menu`) opens whole: no ancestor clips it (a header with
// `overflow: hidden` once cut the session menu to the header's height). The test opens each
// visible menu of the main pages and checks that the corners of its body are on top.
import { expect, type Page, test } from '@playwright/test';
import { logIn, quietShells } from './fake-deck';
import { galleryDeck, LIGHTING, OTTER } from './gallery-scene';

const PAGES = ['#/', '#/fleet', `#/s/${LIGHTING}/${OTTER}`, '#/stations', '#/settings', '#/more'];

/** True when the point is inside the open menu body, so nothing covers or clips it there. */
async function onTop(page: Page, x: number, y: number): Promise<boolean> {
  return page.evaluate(
    ([px, py]) => document.elementFromPoint(px ?? 0, py ?? 0)?.closest('.fd-menu-body') !== null,
    [x, y],
  );
}

for (const hash of PAGES) {
  test(`every menu of ${hash} opens unclipped`, async ({ page }) => {
    await quietShells(page);
    await galleryDeck().install(page);
    await logIn(page);
    await page.evaluate((h) => {
      window.location.hash = h;
    }, hash);
    await page.waitForTimeout(500);
    const summaries = page.locator('details.fd-menu > summary:visible');
    const count = await summaries.count();
    for (let i = 0; i < count; i++) {
      const summary = summaries.nth(i);
      const label = (await summary.getAttribute('aria-label')) ?? (await summary.innerText());
      await summary.click();
      const body = page.locator('details.fd-menu[open] > .fd-menu-body').first();
      await expect(body).toBeVisible();
      const box = await body.boundingBox();
      if (box === null) throw new Error(`${label}: the menu has no box`);
      const view = page.viewportSize() ?? { width: 0, height: 0 };
      // The four inner corners that lie inside the viewport.
      const xs = [box.x + 6, box.x + box.width - 6].filter((x) => x > 0 && x < view.width);
      const ys = [box.y + 6, box.y + box.height - 6].filter((y) => y > 0 && y < view.height);
      for (const x of xs)
        for (const y of ys)
          expect(await onTop(page, x, y), `${hash} menu "${label}" at ${x},${y}`).toBe(true);
      await page.keyboard.press('Escape');
      if (await body.isVisible()) await summary.click();
    }
  });
}
