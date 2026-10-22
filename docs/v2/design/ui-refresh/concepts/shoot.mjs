// Renders the artboards of one concept to PNG (UI refresh, BRIEF.md §6):
//   node docs/v2/design/ui-refresh/concepts/shoot.mjs <folder>
// Playwright comes from web/node_modules, so the script needs no install of its own.
import { mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(new URL('../../../../../web/package.json', import.meta.url));
const { chromium } = require('@playwright/test');

const BOARDS = [
  { shot: 'fleet', file: 'fleet.html', w: 1440, h: 900 },
  { shot: 'fleet-alt', file: 'fleet.html?theme=alt', w: 1440, h: 900 },
  { shot: 'session', file: 'session.html', w: 1440, h: 900 },
  { shot: 'new', file: 'new.html', w: 1440, h: 900 },
  { shot: 'phone-home', file: 'phone-home.html', w: 390, h: 844, phone: true },
  { shot: 'phone-session', file: 'phone-session.html', w: 390, h: 844, phone: true },
];

const folder = process.argv[2];
if (folder === undefined || folder === '') {
  console.error('usage: node shoot.mjs <concept folder>');
  process.exit(2);
}
const dir = new URL(`./${folder}/`, import.meta.url);
mkdirSync(fileURLToPath(new URL('shots/', dir)), { recursive: true });

const browser = await chromium.launch();
try {
  for (const b of BOARDS) {
    const page = await browser.newPage({
      viewport: { width: b.w, height: b.h },
      deviceScaleFactor: b.phone ? 2 : 1,
      isMobile: b.phone === true,
      hasTouch: b.phone === true,
    });
    page.on('pageerror', (e) => console.error(`${b.shot}: page error: ${e.message}`));
    await page.goto(new URL(b.file, dir).href, { waitUntil: 'networkidle' });
    await page.evaluate(() => document.fonts.ready);
    await page.waitForTimeout(250);
    const out = fileURLToPath(new URL(`shots/${b.shot}.png`, dir));
    await page.screenshot({ path: out });
    console.log(`wrote ${out}`);
    await page.close();
  }
} finally {
  await browser.close();
}
