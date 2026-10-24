// Makes every brand file from MARK_PATH (web/src/ui/brand.ts, docs/v2/design/logo.md §3.4, §3.6, §3.7).
// Run: cd web && node ../tools/brand/render.mjs
// The favicon, the app icons, the README lockups and the social preview are committed; never edit them by hand.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const WEB = join(ROOT, 'web');
const BRAND_DIR = join(ROOT, 'docs', 'v2', 'assets', 'brand');
const PUBLIC_DIR = join(WEB, 'public');

// The dark --bg, --text and --dim (this file is outside web/src/styles, so design-check does not scan it).
const INK = '#111821';
const PAPER = '#E9EEF4';
const DIM = '#A5B0BF';

const TAGLINE = 'One control deck that drives Claude Code and Codex sessions on many machines.';

const source = readFileSync(join(WEB, 'src', 'ui', 'brand.ts'), 'utf8');
const match = /export const MARK_PATH =\s*'([^']+)'/.exec(source);
if (match === null) {
  console.error('render: MARK_PATH not found in web/src/ui/brand.ts');
  process.exit(1);
}
const MARK_PATH = match[1];

const num = (n) => String(Number(n.toFixed(4)));

/** The mark alone on a 24-unit box. */
const markSvg = (fill) =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="${fill}" fill-rule="evenodd" d="${MARK_PATH}"/></svg>\n`;

/** The favicon: the bare mark, ink on a light frame, paper on a dark frame. */
const faviconSvg = () =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><style>path{fill:${INK}}@media (prefers-color-scheme:dark){path{fill:${PAPER}}}</style><path fill-rule="evenodd" d="${MARK_PATH}"/></svg>\n`;

/** A square of side `size`, the mark box at fraction `f` of the side, centred. `rounded`: corner radius 22.5 %. */
const tileSvg = (size, f, rounded) => {
  const k = (size * f) / 24;
  const o = (size * (1 - f)) / 2;
  const rx = rounded ? ` rx="${num(size * 0.225)}"` : '';
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">` +
    `<rect width="${size}" height="${size}"${rx} fill="${INK}"/>` +
    `<path transform="translate(${num(o)} ${num(o)}) scale(${num(k)})" fill="${PAPER}" fill-rule="evenodd" d="${MARK_PATH}"/></svg>\n`
  );
};

const inlineMark = (box, fill) =>
  `<svg width="${box}" height="${box}" viewBox="0 0 24 24" style="display:block;flex:none"><path fill="${fill}" fill-rule="evenodd" d="${MARK_PATH}"/></svg>`;

const wordmark = (px, color) =>
  `<span style="font-family:'Mona Sans Variable';font-stretch:115%;font-weight:620;font-size:${px}px;line-height:1;letter-spacing:0;color:${color};white-space:nowrap">Fleet Deck</span>`;

const fontPath = join(
  WEB,
  'node_modules',
  '@fontsource-variable',
  'mona-sans',
  'files',
  'mona-sans-latin-standard-normal.woff2',
);
const fontBase64 = readFileSync(fontPath).toString('base64');
const FONT_FACE = `@font-face { font-family: 'Mona Sans Variable'; src: url(data:font/woff2;base64,${fontBase64}) format('woff2-variations'); font-weight: 200 900; font-stretch: 75% 125%; }`;

const page = (width, height, background, inner) =>
  `<!doctype html><html><head><meta charset="utf-8"><style>${FONT_FACE}html,body{margin:0;padding:0;background:transparent}#shot{position:relative;width:${width}px;height:${height}px;background:${background};overflow:hidden}</style></head><body><div id="shot">${inner}</div></body></html>`;

const lockupHtml = (ink) =>
  page(
    320,
    64,
    'transparent',
    `<div style="position:absolute;left:0;top:4px;display:flex;align-items:center;gap:19px;height:56px">${inlineMark(56, ink)}${wordmark(33, ink)}</div>`,
  );

const socialHtml = () =>
  page(
    1280,
    640,
    INK,
    `<div style="position:absolute;left:112px;top:${280 - 68}px;display:flex;align-items:center;gap:45px;height:136px">${inlineMark(136, PAPER)}${wordmark(80, PAPER)}</div>` +
      `<div style="position:absolute;left:112px;top:392px;width:1056px;font-family:'Mona Sans Variable';font-weight:450;font-size:30px;line-height:1.35;color:${DIM}">${TAGLINE}</div>`,
  );

const written = [];
const write = (path, data) => {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, data);
  written.push(`wrote ${relative(process.cwd(), path)} (${data.length} bytes)`);
};

const require = createRequire(join(WEB, 'package.json'));
const { chromium } = require('playwright');
const browser = await chromium.launch();

/** One PNG of the element #shot of `html`. */
const shoot = async (html, { width, height, scale = 1, transparent = false }) => {
  const context = await browser.newContext({
    viewport: { width, height },
    deviceScaleFactor: scale,
  });
  try {
    const p = await context.newPage();
    await p.setContent(html);
    await p.evaluate(() => document.fonts.ready);
    return await p.locator('#shot').screenshot({ omitBackground: transparent });
  } finally {
    await context.close();
  }
};

const tilePng = (size, f, rounded) =>
  shoot(page(size, size, 'transparent', tileSvg(size, f, rounded).trim()), {
    width: size,
    height: size,
    transparent: true,
  });

try {
  // SVG files.
  write(join(PUBLIC_DIR, 'favicon.svg'), faviconSvg());
  write(join(BRAND_DIR, 'mark.svg'), markSvg(INK));
  write(join(BRAND_DIR, 'mark-dark.svg'), markSvg(PAPER));
  write(join(BRAND_DIR, 'icon.svg'), tileSvg(512, 0.62, true));

  // App icons.
  write(join(PUBLIC_DIR, 'apple-touch-icon.png'), await tilePng(180, 0.62, false));
  write(join(PUBLIC_DIR, 'icon-192.png'), await tilePng(192, 0.62, true));
  write(join(PUBLIC_DIR, 'icon-512.png'), await tilePng(512, 0.62, true));
  write(join(PUBLIC_DIR, 'icon-maskable-512.png'), await tilePng(512, 0.52, false));

  // favicon.ico: three PNG images (16, 32, 48 px) in one ICO.
  const sizes = [16, 32, 48];
  const images = [];
  for (const s of sizes) images.push(await tilePng(s, 0.8, true));
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(sizes.length, 4);
  const entries = [];
  let offset = 6 + 16 * sizes.length;
  sizes.forEach((s, i) => {
    const entry = Buffer.alloc(16);
    entry.writeUInt8(s, 0);
    entry.writeUInt8(s, 1);
    entry.writeUInt8(0, 2);
    entry.writeUInt8(0, 3);
    entry.writeUInt16LE(1, 4);
    entry.writeUInt16LE(32, 6);
    entry.writeUInt32LE(images[i].length, 8);
    entry.writeUInt32LE(offset, 12);
    offset += images[i].length;
    entries.push(entry);
  });
  write(join(PUBLIC_DIR, 'favicon.ico'), Buffer.concat([header, ...entries, ...images]));

  // README lockups (2x of 320x64) and the social preview.
  write(
    join(BRAND_DIR, 'lockup-light.png'),
    await shoot(lockupHtml(INK), { width: 320, height: 64, scale: 2, transparent: true }),
  );
  write(
    join(BRAND_DIR, 'lockup-dark.png'),
    await shoot(lockupHtml(PAPER), { width: 320, height: 64, scale: 2, transparent: true }),
  );
  write(
    join(BRAND_DIR, 'social-preview.png'),
    await shoot(socialHtml(), { width: 1280, height: 640 }),
  );
} finally {
  await browser.close();
}
for (const line of written) console.log(line);
