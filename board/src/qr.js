// Self-contained QR encoder — byte mode, error-correction level M, versions
// 1-16 (up to 450 bytes; a tokenized board URL is ~80).
//
// WHY hand-rolled: the LAN panel must render a scannable code for a URL that
// CONTAINS THE BOARD'S TOKEN. Handing that URL to an external QR service would
// leak a credential that can spawn agents and type into terminals, and the
// board must also work with no internet at all. So: no dependency, no network,
// ~200 lines of ISO/IEC 18004. Structure follows Nayuki's reference algorithm
// (public domain), reduced to the one mode and one EC level this needs.
//
// qrMatrix(text) → { size, modules: boolean[size][size] } | null (too long).
// `true` = dark module. The caller adds the quiet zone (4 modules, required).

const ECC_PER_BLOCK = [0, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28];
const NUM_BLOCKS = [0, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10];
const MAX_VERSION = 16;

// GF(256), primitive polynomial x^8+x^4+x^3+x^2+1 (0x11D)
const EXP = new Uint8Array(512);
const LOG = new Uint8Array(256);
{
  let x = 1;
  for (let i = 0; i < 255; i++) {
    EXP[i] = x;
    LOG[x] = i;
    x <<= 1;
    if (x & 0x100) x ^= 0x11d;
  }
  for (let i = 255; i < 512; i++) EXP[i] = EXP[i - 255];
}
const gmul = (a, b) => (a === 0 || b === 0 ? 0 : EXP[LOG[a] + LOG[b]]);

// generator polynomial for `degree` EC codewords, descending powers
function genPoly(degree) {
  let g = [1];
  for (let i = 0; i < degree; i++) {
    const next = new Array(g.length + 1).fill(0);
    for (let j = 0; j < g.length; j++) {
      next[j] ^= gmul(g[j], 1);
      next[j + 1] ^= gmul(g[j], EXP[i]);
    }
    g = next;
  }
  return g;
}

// Reed-Solomon remainder (systematic encoding)
function eccOf(data, eccLen) {
  const g = genPoly(eccLen);
  const buf = new Uint8Array(data.length + eccLen);
  buf.set(data);
  for (let i = 0; i < data.length; i++) {
    const factor = buf[i];
    if (factor === 0) continue;
    for (let j = 0; j < g.length; j++) buf[i + j] ^= gmul(g[j], factor);
  }
  return Array.from(buf.slice(data.length));
}

const bitAt = (v, i) => ((v >>> i) & 1) !== 0;

// Alignment-pattern centre coordinates (computed, not tabulated)
function alignPositions(version) {
  if (version === 1) return [];
  const n = Math.floor(version / 7) + 2;
  const size = version * 4 + 17;
  const step = Math.ceil((version * 4 + 4) / (n * 2 - 2)) * 2;
  const out = [6];
  for (let pos = size - 7; out.length < n; pos -= step) out.splice(1, 0, pos);
  return out;
}

// 15-bit format info: BCH(15,5) over (EC level bits << 3 | mask), XOR 0x5412.
// EC level M = 0b00.
function formatBits(mask) {
  const data = (0b00 << 3) | mask;
  let rem = data;
  for (let i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
  return ((data << 10) | rem) ^ 0x5412;
}

// 18-bit version info (versions >= 7): BCH(18,6)
function versionBits(version) {
  let rem = version;
  for (let i = 0; i < 12; i++) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
  return (version << 12) | rem;
}

function maskAt(mask, x, y) {
  switch (mask) {
    case 0: return (x + y) % 2 === 0;
    case 1: return y % 2 === 0;
    case 2: return x % 3 === 0;
    case 3: return (x + y) % 3 === 0;
    case 4: return (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0;
    case 5: return ((x * y) % 2) + ((x * y) % 3) === 0;
    case 6: return (((x * y) % 2) + ((x * y) % 3)) % 2 === 0;
    default: return (((x + y) % 2) + ((x * y) % 3)) % 2 === 0;
  }
}

// --- penalty rules (ISO 18004 §8.8.2) — mask selection only ---
const N1 = 3, N2 = 3, N3 = 40, N4 = 10;

function addHistory(run, history, size) {
  if (history[0] === 0) run += size; // the quiet zone counts as a light run
  history.pop();
  history.unshift(run);
}
function countFinderLike(h) {
  const n = h[1];
  const core = n > 0 && h[2] === n && h[3] === n * 3 && h[4] === n && h[5] === n;
  return (core && h[0] >= n * 4 && h[6] >= n ? 1 : 0) + (core && h[6] >= n * 4 && h[0] >= n ? 1 : 0);
}
function terminateAndCount(color, run, history, size) {
  if (color) {
    addHistory(run, history, size);
    run = 0;
  }
  addHistory(run + size, history, size);
  return countFinderLike(history);
}

function penalty(m, size) {
  let score = 0;
  for (let y = 0; y < size; y++) {
    let color = false, run = 0;
    const hist = [0, 0, 0, 0, 0, 0, 0];
    for (let x = 0; x < size; x++) {
      if (m[y][x] === color) {
        run++;
        if (run === 5) score += N1;
        else if (run > 5) score++;
      } else {
        addHistory(run, hist, size);
        if (!color) score += countFinderLike(hist) * N3;
        color = m[y][x];
        run = 1;
      }
    }
    score += terminateAndCount(color, run, hist, size) * N3;
  }
  for (let x = 0; x < size; x++) {
    let color = false, run = 0;
    const hist = [0, 0, 0, 0, 0, 0, 0];
    for (let y = 0; y < size; y++) {
      if (m[y][x] === color) {
        run++;
        if (run === 5) score += N1;
        else if (run > 5) score++;
      } else {
        addHistory(run, hist, size);
        if (!color) score += countFinderLike(hist) * N3;
        color = m[y][x];
        run = 1;
      }
    }
    score += terminateAndCount(color, run, hist, size) * N3;
  }
  for (let y = 0; y < size - 1; y++) {
    for (let x = 0; x < size - 1; x++) {
      const c = m[y][x];
      if (c === m[y][x + 1] && c === m[y + 1][x] && c === m[y + 1][x + 1]) score += N2;
    }
  }
  let dark = 0;
  for (const row of m) for (const c of row) if (c) dark++;
  const total = size * size;
  const k = Math.ceil(Math.abs(dark * 20 - total * 10) / total) - 1;
  return score + k * N4;
}

// --- matrix ---

// Function patterns + the reserved format/version areas. Returns the module
// grid (function modules painted) and the mask of which cells are functional.
function drawFunctions(version) {
  const size = version * 4 + 17;
  const m = Array.from({ length: size }, () => new Array(size).fill(false));
  const fn = Array.from({ length: size }, () => new Array(size).fill(false));
  const set = (x, y, dark) => {
    if (x < 0 || y < 0 || x >= size || y >= size) return;
    m[y][x] = dark;
    fn[y][x] = true;
  };

  for (let i = 0; i < size; i++) {
    set(6, i, i % 2 === 0);
    set(i, 6, i % 2 === 0);
  }
  const finder = (cx, cy) => {
    for (let dy = -4; dy <= 4; dy++) {
      for (let dx = -4; dx <= 4; dx++) {
        const d = Math.max(Math.abs(dx), Math.abs(dy));
        set(cx + dx, cy + dy, d !== 2 && d !== 4);
      }
    }
  };
  finder(3, 3);
  finder(size - 4, 3);
  finder(3, size - 4);

  const pos = alignPositions(version);
  for (let i = 0; i < pos.length; i++) {
    for (let j = 0; j < pos.length; j++) {
      const corner = (i === 0 && j === 0)
        || (i === 0 && j === pos.length - 1)
        || (i === pos.length - 1 && j === 0);
      if (corner) continue; // finder patterns already own those corners
      for (let dy = -2; dy <= 2; dy++) {
        for (let dx = -2; dx <= 2; dx++) {
          set(pos[i] + dx, pos[j] + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
        }
      }
    }
  }

  // reserve the format areas (real bits are written after masking)
  for (let i = 0; i <= 5; i++) set(8, i, false);
  set(8, 7, false); set(8, 8, false); set(7, 8, false);
  for (let i = 9; i < 15; i++) set(14 - i, 8, false);
  for (let i = 0; i < 8; i++) set(size - 1 - i, 8, false);
  for (let i = 8; i < 15; i++) set(8, size - 15 + i, false);
  set(8, size - 8, true); // the always-dark module

  if (version >= 7) {
    const bits = versionBits(version);
    for (let i = 0; i < 18; i++) {
      const bit = bitAt(bits, i);
      const a = size - 11 + (i % 3);
      const b = Math.floor(i / 3);
      set(a, b, bit);
      set(b, a, bit);
    }
  }
  return { size, m, fn };
}

function drawFormat(m, fn, size, mask) {
  const bits = formatBits(mask);
  const set = (x, y, dark) => { m[y][x] = dark; fn[y][x] = true; };
  for (let i = 0; i <= 5; i++) set(8, i, bitAt(bits, i));
  set(8, 7, bitAt(bits, 6));
  set(8, 8, bitAt(bits, 7));
  set(7, 8, bitAt(bits, 8));
  for (let i = 9; i < 15; i++) set(14 - i, 8, bitAt(bits, i));
  for (let i = 0; i < 8; i++) set(size - 1 - i, 8, bitAt(bits, i));
  for (let i = 8; i < 15; i++) set(8, size - 15 + i, bitAt(bits, i));
  set(8, size - 8, true);
}

// zigzag placement, right to left, skipping the vertical timing column
function drawCodewords(m, fn, size, data) {
  let i = 0;
  for (let right = size - 1; right >= 1; right -= 2) {
    if (right === 6) right = 5;
    for (let vert = 0; vert < size; vert++) {
      for (let j = 0; j < 2; j++) {
        const x = right - j;
        const upward = ((right + 1) & 2) === 0;
        const y = upward ? size - 1 - vert : vert;
        if (!fn[y][x] && i < data.length * 8) {
          m[y][x] = bitAt(data[i >>> 3], 7 - (i & 7));
          i++;
        }
      }
    }
  }
}

// data codewords → blocks + ECC, interleaved per ISO 18004 §8.6
function interleave(data, version, rawCodewords) {
  const blocks = NUM_BLOCKS[version];
  const eccLen = ECC_PER_BLOCK[version];
  const shortLen = Math.floor(rawCodewords / blocks);
  const numShort = blocks - (rawCodewords % blocks);
  const built = [];
  for (let i = 0, k = 0; i < blocks; i++) {
    const len = shortLen - eccLen + (i < numShort ? 0 : 1);
    const dat = data.slice(k, k + len);
    k += len;
    const ecc = eccOf(Uint8Array.from(dat), eccLen);
    if (i < numShort) dat.push(0); // pad so every block is the same width
    built.push(dat.concat(ecc));
  }
  const out = [];
  for (let i = 0; i < built[0].length; i++) {
    for (let j = 0; j < built.length; j++) {
      // skip the padding slot the short blocks carry
      if (i !== shortLen - eccLen || j >= numShort) out.push(built[j][i]);
    }
  }
  return out;
}

/** Encode `text` as a QR matrix (byte mode, EC level M). null if too long. */
export function qrMatrix(text) {
  const bytes = new TextEncoder().encode(String(text ?? ''));

  // pick the smallest version that fits, and learn its capacity from the
  // matrix itself (free modules) instead of a second hand-typed table
  let version = 0;
  let raw = 0;
  let dataCw = 0;
  for (let v = 1; v <= MAX_VERSION; v++) {
    const { size, fn } = drawFunctions(v);
    let free = 0;
    for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) if (!fn[y][x]) free++;
    const rawCw = Math.floor(free / 8);
    const cw = rawCw - ECC_PER_BLOCK[v] * NUM_BLOCKS[v];
    const countBits = v < 10 ? 8 : 16;
    if (4 + countBits + bytes.length * 8 <= cw * 8) {
      version = v; raw = rawCw; dataCw = cw;
      break;
    }
  }
  if (!version) return null; // longer than 453 bytes — caller falls back to text

  // bit stream: mode (0100) · length · payload · terminator · pad
  const bits = [];
  const push = (val, n) => { for (let i = n - 1; i >= 0; i--) bits.push((val >>> i) & 1); };
  push(0b0100, 4);
  push(bytes.length, version < 10 ? 8 : 16);
  for (const b of bytes) push(b, 8);
  push(0, Math.min(4, dataCw * 8 - bits.length));
  while (bits.length % 8 !== 0) bits.push(0);

  const codewords = [];
  for (let i = 0; i < bits.length; i += 8) {
    let b = 0;
    for (let j = 0; j < 8; j++) b = (b << 1) | bits[i + j];
    codewords.push(b);
  }
  for (let pad = 0xec; codewords.length < dataCw; pad ^= 0xec ^ 0x11) codewords.push(pad);

  const all = interleave(codewords, version, raw);

  // draw, then pick the mask with the lowest penalty
  let best = null;
  let bestScore = Infinity;
  for (let mask = 0; mask < 8; mask++) {
    const { size, m, fn } = drawFunctions(version);
    drawCodewords(m, fn, size, all);
    for (let y = 0; y < size; y++) {
      for (let x = 0; x < size; x++) {
        if (!fn[y][x] && maskAt(mask, x, y)) m[y][x] = !m[y][x];
      }
    }
    drawFormat(m, fn, size, mask);
    const score = penalty(m, size);
    if (score < bestScore) { bestScore = score; best = { size, modules: m }; }
  }
  return best;
}

/** QR as one SVG path `d` (one subpath per dark module), plus the viewBox side. */
export function qrPath(text, quiet = 4) {
  const qr = qrMatrix(text);
  if (!qr) return null;
  const side = qr.size + quiet * 2;
  let d = '';
  for (let y = 0; y < qr.size; y++) {
    for (let x = 0; x < qr.size; x++) {
      if (qr.modules[y][x]) d += `M${x + quiet} ${y + quiet}h1v1h-1z`;
    }
  }
  return { d, side };
}
