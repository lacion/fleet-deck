// R13 P1: the QR code of a needs-you item. The encoder itself has its tests in `ui/qr.test.ts`.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { QrCode } from './QrCode';
import { answerUrl, QR_QUIET, wallQr } from './qr';

const text = (n: number): string => `https://deck.example/#/s/a/b?request=${'x'.repeat(n)}`;

function cell(grid: boolean[][], y: number, x: number): boolean {
  return grid[y]?.[x] === true;
}

/** The 15 format bits of the copy around the top-left finder, least significant bit first. */
function formatBits(grid: boolean[][]): number {
  const at: [number, number][] = [
    ...[0, 1, 2, 3, 4, 5].map((i): [number, number] => [8, i]),
    [8, 7],
    [8, 8],
    [7, 8],
    ...[9, 10, 11, 12, 13, 14].map((i): [number, number] => [14 - i, 8]),
  ];
  let bits = 0;
  at.forEach(([x, y], i) => {
    if (cell(grid, y, x)) bits |= 1 << i;
  });
  return bits;
}

/** The remainder of `value` divided by the BCH(15,5) generator 0x537. */
function bchRemainder(value: number): number {
  let rem = value;
  for (let i = 14; i >= 10; i -= 1) if ((rem >> i) & 1) rem ^= 0x537 << (i - 10);
  return rem;
}

describe('the answer URL', () => {
  test('it points at the session route with the request id', () => {
    expect(answerUrl('https://deck.example', 'st-1', 'se-9', 'req 1')).toBe(
      'https://deck.example/#/s/st-1/se-9?request=req%201',
    );
  });

  test('it holds no token', () => {
    const url = answerUrl('https://deck.example:7420', 'station-a', 'session-1', 'r1');
    expect(url).not.toContain('#login=');
    expect(url).not.toContain('token=');
  });
});

describe('the symbol', () => {
  test('the finder patterns and the timing lines are in place', () => {
    const code = wallQr(text(10));
    if (code === null) throw new Error('the URL must be encodable');
    const { size, modules } = code.matrix;
    const finder = (x0: number, y0: number) => {
      for (let y = 0; y < 7; y += 1) {
        for (let x = 0; x < 7; x += 1) {
          const edge = x === 0 || y === 0 || x === 6 || y === 6;
          const core = x >= 2 && x <= 4 && y >= 2 && y <= 4;
          expect(cell(modules, y0 + y, x0 + x)).toBe(edge || core);
        }
      }
    };
    finder(0, 0);
    finder(size - 7, 0);
    finder(0, size - 7);
    for (let i = 8; i < size - 8; i += 1) {
      expect(cell(modules, 6, i)).toBe(i % 2 === 0);
      expect(cell(modules, i, 6)).toBe(i % 2 === 0);
    }
  });

  test('the format bits are a valid BCH code word with level M', () => {
    const code = wallQr(text(10));
    if (code === null) throw new Error('the URL must be encodable');
    const bits = formatBits(code.matrix.modules) ^ 0x5412;
    expect(bchRemainder(bits)).toBe(0);
    expect(bits >> 13).toBe(0); // the level M bits are 00
    expect((bits >> 10) & 7).toBeLessThan(8); // the mask number
  });

  test('the version follows the length: 17, 100 and 200 bytes', () => {
    const sizeOf = (bytes: number) => {
      const url = 'x'.repeat(bytes);
      expect(new TextEncoder().encode(url).length).toBe(bytes);
      return wallQr(url)?.matrix.size;
    };
    expect(sizeOf(17)).toBe(25); // version 2 (26 bytes at level M)
    expect(sizeOf(100)).toBe(41); // version 6 (106 bytes)
    expect(sizeOf(200)).toBe(57); // version 10 (213 bytes)
  });

  test('a text longer than the encoder takes gives no symbol', () => {
    expect(wallQr('x'.repeat(600))).toBeNull();
  });

  test('the quiet zone is 4 modules', () => {
    const code = wallQr(text(5));
    if (code === null) throw new Error('the URL must be encodable');
    expect(QR_QUIET).toBe(4);
    expect(code.side).toBe(code.matrix.size + 8);
  });
});

describe('QrCode', () => {
  test('an SVG with the label of the URL', () => {
    const url = answerUrl('https://deck.example', 'a', 'b', 'r1');
    const html = renderToStaticMarkup(<QrCode url={url} />);
    expect(html).toContain('<svg');
    expect(html).toContain(`aria-label="QR code for ${url}"`);
    expect(html).not.toContain('<button');
  });

  test('a URL that is too long shows nothing', () => {
    expect(renderToStaticMarkup(<QrCode url={'x'.repeat(600)} />)).toBe('');
  });
});
