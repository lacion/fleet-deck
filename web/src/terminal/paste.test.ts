import { describe, expect, test } from 'bun:test';
import { extOf, insertText, MAX_PASTE_BYTES, newUploadId, prepareImage, toBase64 } from './paste';

describe('extOf', () => {
  test('the four image types map to their extension, all others to null', () => {
    expect(extOf('image/png')).toBe('png');
    expect(extOf('image/jpeg')).toBe('jpg');
    expect(extOf('image/gif')).toBe('gif');
    expect(extOf('image/webp')).toBe('webp');
    for (const mime of ['image/bmp', 'image/avif', 'text/plain', '', 'IMAGE/PNG', 'image/jpg']) {
      expect(extOf(mime)).toBeNull();
    }
  });
});

describe('insertText', () => {
  test('a plain path goes as it is, with a space after it', () => {
    expect(insertText('/tmp/fleet-deck/paste-ab12.png')).toBe('/tmp/fleet-deck/paste-ab12.png ');
    expect(insertText('/home/a_b/x+y-z.webp')).toBe('/home/a_b/x+y-z.webp ');
  });

  test('a path with a space or another special character is single-quoted', () => {
    expect(insertText('/tmp/my dir/a.png')).toBe("'/tmp/my dir/a.png' ");
    expect(insertText('/tmp/a$b;c.png')).toBe("'/tmp/a$b;c.png' ");
  });

  test('a single quote in the path is closed, escaped and opened again', () => {
    expect(insertText("/tmp/it's.png")).toBe("'/tmp/it'\\''s.png' ");
  });

  test('a control character gives null', () => {
    for (const bad of [
      '/tmp/a\nb.png',
      '/tmp/a\rb',
      '/tmp/a\u0000b',
      '/tmp/a\u001bb',
      '/tmp/a\u007fb',
    ]) {
      expect(insertText(bad)).toBeNull();
    }
  });

  test('an empty path is quoted, never empty text', () => {
    expect(insertText('')).toBe("'' ");
  });
});

describe('toBase64', () => {
  test('encodes bytes in standard base64', () => {
    expect(toBase64(new Uint8Array([]))).toBe('');
    expect(toBase64(new Uint8Array([0, 1, 2]))).toBe('AAEC');
    expect(toBase64(new Uint8Array([0xff, 0xfe]))).toBe('//4=');
    expect(toBase64(new TextEncoder().encode('Man'))).toBe('TWFu');
  });

  test('a large input matches Buffer and does not overflow the stack', () => {
    const bytes = new Uint8Array(MAX_PASTE_BYTES).map((_, i) => (i * 31) % 256);
    expect(toBase64(bytes)).toBe(Buffer.from(bytes).toString('base64'));
  });
});

describe('newUploadId', () => {
  test('is 32 hex characters, which the deck accepts, and differs each time', () => {
    const a = newUploadId();
    expect(a).toMatch(/^[0-9a-f]{32}$/);
    expect(a).toMatch(/^[A-Za-z0-9_-]{8,64}$/);
    expect(newUploadId()).not.toBe(a);
  });
});

describe('prepareImage', () => {
  test('a small supported image goes through unchanged', async () => {
    const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 1, 2, 3]);
    const out = await prepareImage(new Blob([png], { type: 'image/png' }));
    expect(out).toEqual({ ext: 'png', bytes: png });
  });

  test('an image at the size limit goes through', async () => {
    const bytes = new Uint8Array(MAX_PASTE_BYTES);
    const out = await prepareImage(new Blob([bytes], { type: 'image/webp' }));
    expect('ext' in out && out.ext).toBe('webp');
    expect('bytes' in out && out.bytes.length).toBe(MAX_PASTE_BYTES);
  });
});
