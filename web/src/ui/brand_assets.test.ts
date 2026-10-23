// The generated brand files (tools/brand/render.mjs) carry the current mark and have their sizes.
import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { MARK_PATH } from './brand';

const WEB = join(import.meta.dir, '..', '..');
const ROOT = join(WEB, '..');
const PUBLIC = join(WEB, 'public');
const BRAND = join(ROOT, 'docs', 'v2', 'assets', 'brand');

/** The width and height of a PNG file, from its IHDR chunk (bytes 16-23, big-endian). */
const pngSize = (path: string): { width: number; height: number } => {
  const bytes = readFileSync(path);
  return { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
};

describe('the brand files', () => {
  test('the brand SVGs carry the mark path', () => {
    const files = [
      join(PUBLIC, 'favicon.svg'),
      join(BRAND, 'mark.svg'),
      join(BRAND, 'mark-dark.svg'),
      join(BRAND, 'icon.svg'),
    ];
    for (const file of files) {
      const text = readFileSync(file, 'utf8');
      expect(text, file).toContain(`d="${MARK_PATH}"`);
      expect(text, file).toContain('fill-rule="evenodd"');
    }
    expect(readFileSync(join(PUBLIC, 'favicon.svg'), 'utf8')).toContain(
      'prefers-color-scheme:dark',
    );
  });

  test('the manifest lists icons that exist at their sizes', () => {
    const manifest = JSON.parse(readFileSync(join(PUBLIC, 'manifest.webmanifest'), 'utf8')) as {
      name: string;
      display: string;
      start_url: string;
      icons: { src: string; sizes: string; type: string; purpose: string }[];
    };
    expect(manifest.name).toBe('Fleet Deck');
    expect(manifest.display).toBe('browser');
    expect(manifest.start_url).toBe('./');
    for (const icon of manifest.icons.filter((i) => i.type === 'image/png')) {
      const path = join(PUBLIC, icon.src);
      expect(existsSync(path), icon.src).toBe(true);
      const { width, height } = pngSize(path);
      expect(`${width}x${height}`, icon.src).toBe(icon.sizes);
    }
    expect(manifest.icons.filter((i) => i.purpose === 'maskable')).toHaveLength(1);
  });

  test('the other PNG files have their sizes', () => {
    expect(pngSize(join(PUBLIC, 'apple-touch-icon.png'))).toEqual({ width: 180, height: 180 });
    expect(pngSize(join(BRAND, 'lockup-light.png'))).toEqual({ width: 640, height: 128 });
    expect(pngSize(join(BRAND, 'lockup-dark.png'))).toEqual({ width: 640, height: 128 });
    expect(pngSize(join(BRAND, 'social-preview.png'))).toEqual({ width: 1280, height: 640 });
  });

  test('favicon.ico holds 16, 32 and 48 px PNG images', () => {
    const ico = readFileSync(join(PUBLIC, 'favicon.ico'));
    expect(ico.readUInt16LE(2)).toBe(1);
    expect(ico.readUInt16LE(4)).toBe(3);
    [16, 32, 48].forEach((size, i) => {
      const entry = 6 + 16 * i;
      expect(ico.readUInt8(entry)).toBe(size);
      expect(ico.readUInt8(entry + 1)).toBe(size);
      const offset = ico.readUInt32LE(entry + 12);
      expect([...ico.subarray(offset, offset + 4)]).toEqual([0x89, 0x50, 0x4e, 0x47]);
    });
  });

  test('index.html links the icons and the manifest', () => {
    const html = readFileSync(join(WEB, 'index.html'), 'utf8');
    for (const line of [
      '<link rel="icon" href="/favicon.ico" sizes="16x16 32x32 48x48" />',
      '<link rel="icon" href="/favicon.svg" type="image/svg+xml" />',
      '<link rel="apple-touch-icon" href="/apple-touch-icon.png" />',
      '<link rel="manifest" href="/manifest.webmanifest" />',
    ]) {
      expect(html).toContain(line);
    }
  });

  test('the old icon is gone', () => {
    expect(existsSync(join(ROOT, 'docs', 'v2', 'assets', 'fleet-deck-icon.png'))).toBe(false);
  });
});
