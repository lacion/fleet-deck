import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { Markdown } from '../chat/Markdown';
import { splitTokens } from './tokens';

const known = new Set(['c-abc234']);

describe('splitTokens', () => {
  test('a full known id becomes a chip', () => {
    expect(splitTokens('see c-abc234 now', known)).toEqual([
      { text: 'see ' },
      { shortId: 'c-abc234', raw: 'c-abc234' },
      { text: ' now' },
    ]);
  });

  test('a bracketed id is a chip with its brackets as raw text', () => {
    expect(splitTokens('[c-abc234]', known)).toEqual([{ shortId: 'c-abc234', raw: '[c-abc234]' }]);
  });

  test('a too short id, a longer word, a prefix word and an unknown id stay text', () => {
    for (const text of ['c-abc23', 'c-abc234x', 'xc-abc234', 'c-zzz999', 'c-abc234-b']) {
      expect(splitTokens(text, known)).toEqual([{ text }]);
    }
  });

  test('a prefix of a known id is not a match', () => {
    expect(splitTokens('c-abc2345', new Set(['c-abc234']))).toEqual([{ text: 'c-abc2345' }]);
  });

  test('two chips and the text between them', () => {
    const two = new Set(['c-abc234', 'c-xyz789']);
    expect(splitTokens('c-abc234, c-xyz789.', two)).toEqual([
      { shortId: 'c-abc234', raw: 'c-abc234' },
      { text: ', ' },
      { shortId: 'c-xyz789', raw: 'c-xyz789' },
      { text: '.' },
    ]);
  });

  test('an unknown id between known ones merges into one text piece', () => {
    expect(splitTokens('a c-zzz999 b', known)).toEqual([{ text: 'a c-zzz999 b' }]);
  });
});

const md = (text: string) =>
  renderToStaticMarkup(<Markdown text={text} comments={known} onComment={() => {}} />);

describe('Markdown chips', () => {
  test('a known token renders as a chip span', () => {
    const html = md('look at c-abc234 please');
    expect(html).toContain('data-comment="c-abc234"');
    expect(html).toContain('<button type="button" class="fd-cchip"');
  });

  test('without onComment the chip is a span', () => {
    const html = renderToStaticMarkup(<Markdown text="c-abc234" comments={known} />);
    expect(html).toContain('<span class="fd-cchip" data-comment="c-abc234">');
  });

  test('a bracketed token renders as a chip', () => {
    // The chip shows the short id; the brackets are the raw text that it replaces.
    const html = md('see [c-abc234]');
    expect(html).toContain('data-comment="c-abc234"');
    expect(html).not.toContain('[c-abc234]');
  });

  test('no chip inside a link text, inline code or a code block', () => {
    expect(md('[c-abc234](https://example.com)')).not.toContain('data-comment');
    expect(md('`c-abc234`')).not.toContain('data-comment');
    expect(md('```\nc-abc234\n```')).not.toContain('data-comment');
    expect(md('[c-abc234][ref]\n\n[ref]: https://example.com')).not.toContain('data-comment');
  });

  test('a chip works inside a list item and emphasis', () => {
    expect(md('- item c-abc234')).toContain('data-comment="c-abc234"');
    expect(md('*c-abc234*')).toContain('data-comment="c-abc234"');
  });

  test('an unknown token stays text', () => {
    expect(md('c-zzz999')).not.toContain('data-comment');
  });

  test('without comments the Markdown is unchanged', () => {
    const html = renderToStaticMarkup(<Markdown text="c-abc234" />);
    expect(html).not.toContain('data-comment');
    expect(html).toContain('c-abc234');
  });

  test('a span that is not a chip renders as a plain span', () => {
    // The raw HTML is dropped (skipHtml); nothing becomes an element.
    const html = md('<span data-comment="c-abc234">x</span>');
    expect(html).not.toContain('<span');
  });

  test('hostile Markdown and HTML in a comment body render as text', () => {
    const html = renderToStaticMarkup(
      <Markdown
        text={
          '<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n![pic](https://example.com/p.png)\n\n[x](javascript:alert(1))'
        }
        comments={known}
      />,
    );
    expect(html).not.toContain('<script');
    expect(html).not.toContain('<img');
    expect(html).not.toContain('onerror');
    expect(html).not.toContain('href="javascript:');
  });
});
