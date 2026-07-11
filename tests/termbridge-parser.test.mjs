import test from 'node:test';
import assert from 'node:assert/strict';
import { StringDecoder } from 'node:string_decoder';
import { ControlModeParser, parseControlChunk, unescapeControlData } from '../scripts/fleetd/termbridge.mjs';

test('control parser incrementally matches response blocks across awkward chunks', () => {
  const parser = new ControlModeParser();
  const transcript = '%begin 171 9 0\nfirst\n%end 171 8 0\nsecond\n%end 171 9 0\n%begin 172 10 0\nbad target\n%error 172 10 0\n';
  const chunks = [transcript.slice(0, 2), transcript.slice(2, 19), transcript.slice(19, 37), transcript.slice(37, 63), transcript.slice(63)];
  const events = chunks.flatMap(chunk => parser.feed(chunk));
  assert.deepEqual(events, [
    { type: 'response', key: '171:9', time: '171', number: '9', ok: true, lines: ['first', '%end 171 8 0', 'second'] },
    { type: 'response', key: '172:10', time: '172', number: '10', ok: false, lines: ['bad target'] },
  ]);
});

test('control parser unescapes output bytes, filters nothing, and ignores unknown notifications', () => {
  // Fed as a Buffer, exactly as the control client's stdout arrives: the
  // parser is a BYTE pipe and must return the pane's bytes untouched.
  const events = parseControlChunk(Buffer.from('%layout-change @1 abc\n%output %7 A\\033B\\134C café\n%window-close @4\n%session-changed $0 fleetdeck-4711\n%exit session gone\n', 'utf8'));
  assert.equal(events.length, 4);
  assert.equal(events[0].type, 'output');
  assert.equal(events[0].pane, '%7');
  assert.deepEqual(events[0].data, Buffer.from('A\u001bB\\C café', 'utf8'));
  assert.deepEqual(events[1], { type: 'window-close', window: '@4' });
  assert.deepEqual(events[2], { type: 'session-changed', session: '$0', name: 'fleetdeck-4711' });
  assert.deepEqual(events[3], { type: 'exit', reason: 'session gone' });
});

test('control parser preserves a multibyte UTF-8 character split across Buffer chunks', () => {
  const parser = new ControlModeParser();
  const wire = Buffer.from('%output %1 snowman ☃\n');
  const split = wire.indexOf(Buffer.from('☃')) + 1;
  assert.deepEqual(parser.feed(wire.subarray(0, split)), []);
  const [event] = parser.feed(wire.subarray(split));
  assert.equal(event.data.toString('utf8'), 'snowman ☃');
});

test('octal decoder retains incomplete/unknown backslashes and decodes control bytes', () => {
  assert.deepEqual(unescapeControlData('x\\001\\033\\134y\\12'), Buffer.from('x\x01\x1b\\y\\12'));
});

test('a glyph split across two %output notifications survives intact', () => {
  // THE regression. tmux flushes pane output as bytes land, so one box-drawing
  // character (3 bytes, and Claude's TUI is built from them) can straddle two
  // %output lines. Decoding the control stream as UTF-8 text used to meet the
  // first two bytes, then the protocol's own newline, and burn the character
  // down to U+FFFD — two junk cells that shoved the rest of the row sideways
  // and wrapped the line. The parser must stay byte-exact and let the viewer's
  // decoder rejoin the halves.
  const glyph = Buffer.from('─', 'utf8');
  assert.equal(glyph.length, 3, 'sanity: the box rule really is multi-byte');
  const wire = Buffer.concat([
    Buffer.from('%output %1 ', 'latin1'), glyph.subarray(0, 2), Buffer.from('\n', 'latin1'),
    Buffer.from('%output %1 ', 'latin1'), glyph.subarray(2), Buffer.from('\n', 'latin1'),
  ]);
  const events = parseControlChunk(wire);
  assert.equal(events.length, 2);
  assert.deepEqual(Buffer.concat(events.map(e => e.data)), glyph,
    'the parser must not re-encode, drop or mangle the split bytes');

  const decoder = new StringDecoder('utf8');
  const text = events.map(e => decoder.write(e.data)).join('') + decoder.end();
  assert.equal(text, '─', 'a viewer-side decoder reassembles the character');
  assert.ok(!text.includes('\ufffd'), 'no replacement character may reach the browser');
});
