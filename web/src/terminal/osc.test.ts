import { describe, expect, test } from 'bun:test';
import { OSC52_MAX, oscCopyText } from './osc';

const b64 = (text: string) => Buffer.from(text, 'utf8').toString('base64');

describe('oscCopyText', () => {
  test('decodes the base64 payload', () => {
    expect(oscCopyText('c;aGVsbG8=')).toBe('hello');
  });

  test('decodes UTF-8 text', () => {
    expect(oscCopyText(`c;${b64('héllo ✓')}`)).toBe('héllo ✓');
  });

  test('ignores a read request', () => {
    expect(oscCopyText('c;?')).toBeNull();
  });

  test('ignores invalid base64', () => {
    expect(oscCopyText('c;!!!not base64')).toBeNull();
    expect(oscCopyText('c;aGVsbG8')).toBeNull();
  });

  test('ignores data without a payload', () => {
    expect(oscCopyText('c')).toBeNull();
    expect(oscCopyText('c;')).toBeNull();
  });

  test('ignores a text over the limit and keeps one at the limit', () => {
    expect(oscCopyText(`c;${b64('a'.repeat(OSC52_MAX + 1))}`)).toBeNull();
    expect(oscCopyText(`c;${b64('a'.repeat(OSC52_MAX))}`)).toHaveLength(OSC52_MAX);
  });
});
