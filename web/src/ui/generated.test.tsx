import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { digestLine, digestLines, GeneratedMark } from './generated';

describe('generated text (DL-167)', () => {
  test('the mark says that the text is generated', () => {
    const html = renderToStaticMarkup(<GeneratedMark />);
    expect(html).toContain('class="fd-generated"');
    expect(html).toContain('>generated<');
  });

  test('a digest loses its list markers and blank lines', () => {
    const digest = '- Read the router\n\n* Fixed the redirect\n  • Ran the tests  \n';
    expect(digestLines(digest)).toEqual(['Read the router', 'Fixed the redirect', 'Ran the tests']);
    expect(digestLine(digest)).toBe('Read the router');
  });

  test('no digest is no line', () => {
    expect(digestLines(undefined)).toEqual([]);
    expect(digestLines(null)).toEqual([]);
    expect(digestLine('')).toBeNull();
    expect(digestLine(undefined)).toBeNull();
  });
});
