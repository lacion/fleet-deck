import test from 'node:test';
import assert from 'node:assert/strict';
import { sanitizePaneText } from '../scripts/fleetd/spawn.mjs';

// BRACKETED-PASTE BREAKOUT (CONTRACT): pasteText delivers mail with `-p`, so
// tmux wraps the buffer in ESC[200~ … ESC[201~. Verbatim mail content carrying
// its own END marker would close the bracket early and turn the rest into live
// keystrokes the daemon then submits. sanitizePaneText is the pure chokepoint
// that neutralizes this; these assertions pin the exact guarantee it makes.
const ESC = '\x1b';

test('the bracketed-paste END marker is stripped whole', () => {
  const out = sanitizePaneText(`${ESC}[201~`);
  assert.ok(!out.includes(ESC), 'no ESC byte may survive');
  assert.equal(out.includes(`${ESC}[201~`), false, 'the armed marker is gone');
  assert.equal(out, '', 'nothing legible was in it to keep');
});

test('a breakout payload cannot smuggle a live END marker past the sanitizer', () => {
  // The attack: content that closes the bracket early so "/exit\n" lands as
  // live keystrokes the daemon's own sendEnter then submits.
  const out = sanitizePaneText(`hi${ESC}[201~/exit\n`);
  assert.ok(!out.includes(ESC), 'the ESC that arms the marker is gone');
  assert.equal(out.includes('201~'), false, 'no raw 201~ remains afterward');
  assert.equal(out, 'hi/exit\n', 'legible text survives; only the escape is removed');
});

test('the START marker and a reconstitution overlap are both neutralized', () => {
  assert.equal(sanitizePaneText(`${ESC}[200~payload`), 'payload');
  // A crafted overlap that re-forms a marker after ONE pass: the delete loop
  // runs until stable and the C0 strip removes any leftover ESC regardless.
  const out = sanitizePaneText(`${ESC}[20${ESC}[201~1~`);
  assert.ok(!out.includes(ESC), 'no ESC survives reconstitution');
  assert.equal(out.includes('201~'), false, 'no marker text is reconstituted');
});

test('a bare ESC that could open a fresh control sequence is removed', () => {
  assert.equal(sanitizePaneText(`a${ESC}b`), 'ab');
  assert.equal(sanitizePaneText(`${ESC}[31mred${ESC}[0m`), '[31mred[0m');
});

test('other C0 control bytes are stripped but tab and newline are kept', () => {
  assert.equal(sanitizePaneText('a\x00b\x07c\x1fd\x7fe'), 'abcde');
  assert.equal(sanitizePaneText('line1\nline2\tcol'), 'line1\nline2\tcol');
});

test('C1 controls (incl. the 8-bit CSI U+009B) are stripped', () => {
  const CSI = String.fromCharCode(0x9b); // C1 CSI — the single-byte form of ESC[
  const out = sanitizePaneText('hi' + CSI + '201~/exit');
  assert.ok(![...out].some((ch) => ch.codePointAt(0) >= 0x80 && ch.codePointAt(0) <= 0x9f), 'no C1 control survives');
  assert.equal(out, 'hi201~/exit', 'the 8-bit CSI is removed; legible text is kept');
  // The whole C1 band (0x80–0x9f) goes; text above U+00FF is untouched.
  assert.equal(sanitizePaneText('a' + String.fromCharCode(0x80) + String.fromCharCode(0x9f) + 'b'), 'ab');
  assert.equal(sanitizePaneText('café 世界 🚀'), 'café 世界 🚀');
});

test('CRLF and a lone CR normalize to LF', () => {
  assert.equal(sanitizePaneText('a\r\nb\rc'), 'a\nb\nc');
});

test('plain unicode text (emoji, CJK) passes through unchanged', () => {
  const text = 'hello 世界 🚀 café — naïve résumé';
  assert.equal(sanitizePaneText(text), text);
});

test('non-string input is coerced rather than thrown on', () => {
  assert.equal(sanitizePaneText(12345), '12345');
  assert.equal(sanitizePaneText(null), 'null');
});
