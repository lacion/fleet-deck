// tests/board-qr.test.mjs
//
// Pure tests for the board's hand-rolled QR encoder (board/src/qr.js).
//
// This encoder turns the LAN token URL into a scannable code with no external
// dependency and no network (handing a URL that CONTAINS the board's token to a
// QR web service would leak a credential that can spawn agents). A silent bug
// here is invisible to the eye but yields an unscannable / wrong-credential
// code, so this file pins the structural invariants of a real QR symbol.
//
// The module's only export is qrPath(text, quiet) -> { d, side } | null, an SVG
// path string with one `M<x> <y>h1v1h-1z` subpath per DARK module. That path is
// losslessly reconstructible into the module matrix, so we rebuild the grid and
// assert on it directly — the internal qrMatrix() is not exported, but nothing
// is lost by going through qrPath. board/package.json is "type": "module", so it
// loads under `node --test` with no bundler.

import test from 'node:test';
import assert from 'node:assert/strict';

import { qrPath } from '../board/src/qr.js';

// The 7x7 finder pattern that sits at three corners of every QR symbol
// (ISO/IEC 18004). This is the stable structural invariant we assert on.
const FINDER = [
  [1, 1, 1, 1, 1, 1, 1],
  [1, 0, 0, 0, 0, 0, 1],
  [1, 0, 1, 1, 1, 0, 1],
  [1, 0, 1, 1, 1, 0, 1],
  [1, 0, 1, 1, 1, 0, 1],
  [1, 0, 0, 0, 0, 0, 1],
  [1, 1, 1, 1, 1, 1, 1],
];

// Reconstruct the boolean module grid from the SVG path. Using quiet = 0 makes
// the path coordinates equal to raw module coordinates.
function toGrid(text, quiet = 0) {
  const res = qrPath(text, quiet);
  if (!res) return null;
  const size = res.side - 2 * quiet;
  const grid = Array.from({ length: size }, () => new Array(size).fill(false));
  const re = /M(\d+) (\d+)h1v1h-1z/g;
  let m;
  let count = 0;
  while ((m = re.exec(res.d)) !== null) {
    const x = Number(m[1]) - quiet;
    const y = Number(m[2]) - quiet;
    grid[y][x] = true;
    count += 1;
  }
  // sanity: the path is nothing but these subpaths — no stray drawing commands
  assert.equal(res.d.replace(re, ''), '', 'path is only h1v1 module subpaths');
  return { grid, size, darkCount: count, res };
}

function finderMatches(grid, x0, y0) {
  for (let dy = 0; dy < 7; dy++) {
    for (let dx = 0; dx < 7; dx++) {
      if (Boolean(grid[y0 + dy][x0 + dx]) !== Boolean(FINDER[dy][dx])) return false;
    }
  }
  return true;
}

// A valid QR dimension is 4*version + 17 (version 1..16 -> 21..81).
function isValidQrSize(size) {
  return size >= 21 && size <= 81 && (size - 17) % 4 === 0;
}

// ---------------------------------------------------- well-formed short symbol

test('a known short ASCII string encodes to a well-formed square matrix', () => {
  const g = toGrid('HELLO');
  assert.ok(g, 'HELLO must be encodable');
  assert.ok(isValidQrSize(g.size), `size ${g.size} must be a valid QR dimension`);
  // HELLO is 5 bytes: 4 + 8 + 40 = 52 data bits, fits version 1 (size 21).
  assert.equal(g.size, 21, 'a 5-byte payload is a version-1 symbol');
  // the grid is square with the reported side
  assert.equal(g.grid.length, g.size);
  for (const row of g.grid) assert.equal(row.length, g.size);
});

test('finder patterns are present at all three corners', () => {
  const g = toGrid('HELLO');
  const s = g.size;
  assert.ok(finderMatches(g.grid, 0, 0), 'top-left finder');
  assert.ok(finderMatches(g.grid, s - 7, 0), 'top-right finder');
  assert.ok(finderMatches(g.grid, 0, s - 7), 'bottom-left finder');
});

test('the symbol carries data beyond the three finders (not an empty frame)', () => {
  const g = toGrid('HELLO');
  // three finders alone are 3 * 24 = 72 dark modules; a real symbol has far
  // more once timing, format and data modules are painted.
  assert.ok(g.darkCount > 72, `expected data modules, got ${g.darkCount} dark`);
});

// ------------------------------------------------------------------ determinism

test('same input yields identical output (deterministic mask selection)', () => {
  const a = qrPath('HELLO');
  const b = qrPath('HELLO');
  assert.deepEqual(a, b);
  // and independently reconstructed grids agree
  assert.deepEqual(toGrid('board-token-42').grid, toGrid('board-token-42').grid);
});

// ---------------------------------------------------- realistic LAN token URL

test('a realistic token URL encodes without throwing and stays well-formed', () => {
  const url = 'http://192.168.1.5:4711/?t=' + 'a1b2c3d4e5f60718293a4b5c6d7e8f90';
  assert.equal(url.length, 27 + 32);
  const g = toGrid(url);
  assert.ok(g, 'the token URL must be encodable');
  assert.ok(isValidQrSize(g.size), `size ${g.size} must be a valid QR dimension`);
  // ~59 bytes needs more than version 1's 16 data codewords, so it grows.
  assert.ok(g.size > 21, 'a ~59-byte URL needs a larger symbol than version 1');
  assert.ok(finderMatches(g.grid, 0, 0), 'top-left finder');
  assert.ok(finderMatches(g.grid, g.size - 7, 0), 'top-right finder');
  assert.ok(finderMatches(g.grid, 0, g.size - 7), 'bottom-left finder');
});

// -------------------------------------------------------------- quiet zone / API

test('the quiet zone widens the SVG viewBox but not the module grid', () => {
  const bare = qrPath('HELLO', 0);
  const quilted = qrPath('HELLO'); // default quiet = 4
  assert.equal(quilted.side, bare.side + 8, 'default 4-module quiet zone on each side');
  // reconstructing with the matching quiet offset yields the same modules
  assert.deepEqual(toGrid('HELLO', 0).grid, toGrid('HELLO', 4).grid);
});

// ---------------------------------------------------------------- edge inputs

test('empty and nullish inputs still encode (byte mode, length 0) — no throw', () => {
  for (const empty of ['', null, undefined]) {
    const g = toGrid(empty);
    assert.ok(g, `${String(empty)} should encode to a valid symbol`);
    assert.ok(isValidQrSize(g.size));
    assert.ok(finderMatches(g.grid, 0, 0), 'top-left finder present even when empty');
  }
});

test('over-capacity input returns null (caller falls back to text)', () => {
  // versions 1..16 top out around 450 bytes; 600 bytes cannot fit.
  const tooLong = 'x'.repeat(600);
  assert.equal(qrPath(tooLong), null);
});
