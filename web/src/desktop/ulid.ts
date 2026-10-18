// R13 P5a (design §2.2): the capture ID is a canonical ULID that the board makes before it sends:
// 48 bits of millisecond time and 80 random bits, Crockford base32, uppercase, 26 characters.
const ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

/** Fills the bytes with random values; `crypto.getRandomValues` in the browser. */
export type Fill = (bytes: Uint8Array) => void;

const secure: Fill = (bytes) => {
  crypto.getRandomValues(bytes);
};

/** A new ULID. `now` is Unix milliseconds. The first character is 0-7 (the time fits 48 bits). */
export function newUlid(now: number = Date.now(), fill: Fill = secure): string {
  let time = '';
  let rest = Math.max(0, Math.floor(now));
  for (let i = 0; i < 10; i += 1) {
    time = ALPHABET.charAt(rest % 32) + time;
    rest = Math.floor(rest / 32);
  }
  const bytes = new Uint8Array(10);
  fill(bytes);
  let bits = 0n;
  for (const b of bytes) bits = (bits << 8n) | BigInt(b);
  let random = '';
  for (let i = 0; i < 16; i += 1) {
    random = ALPHABET.charAt(Number(bits & 31n)) + random;
    bits >>= 5n;
  }
  return time + random;
}
