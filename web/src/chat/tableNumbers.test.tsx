import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { Markdown } from './Markdown';
import { isNumberCell } from './tableNumbers';

const html = (text: string) => renderToStaticMarkup(<Markdown text={text} />);

describe('number cells', () => {
  test('a count, a share, a time, a size and money are numbers', () => {
    for (const text of [
      '779',
      '1,286',
      '55%',
      '16 h',
      '12 min',
      '−3.5',
      '+9',
      '9.4K',
      '1.2 GB',
      '$1.20',
    ]) {
      expect(isNumberCell(text)).toBe(true);
    }
  });

  test('words, ids, dates and versions are not', () => {
    for (const text of [
      'ES-H03',
      'v2',
      '2026-10-22',
      '3e21551',
      '#128',
      'about 750',
      '10:07',
      '',
    ]) {
      expect(isNumberCell(text)).toBe(false);
    }
  });
});

describe('the number columns of a chat table', () => {
  const table = [
    '| Post | Age | Plays | Avg watch |',
    '| --- | --- | --- | --- |',
    '| ES-H03 | 16 h | 779 | 55% |',
    '| US-H01 Tyler (H1) | 22 h | **778** | — |',
  ].join('\n');

  test('every cell of a number column, its header too, has data-num', () => {
    const out = html(table);
    expect(out).toContain('<th>Post</th>');
    expect(out).toContain('<th data-num="">Age</th>');
    expect(out).toContain('<th data-num="">Plays</th>');
    expect(out).toContain('<td>ES-H03</td>');
    expect(out).toContain('<td data-num="">16 h</td>');
    // Bold text counts by its words; a dash is a blank cell.
    expect(out).toContain('<td data-num=""><strong>778</strong></td>');
    expect(out).toContain('<td data-num="">—</td>');
  });

  test('a column with one word, or with blank cells only, stays as it is', () => {
    const out = html(
      ['| Name | Size | Note |', '| --- | --- | --- |', '| a | 12 | |', '| b | big | — |'].join(
        '\n',
      ),
    );
    expect(out).not.toContain('data-num');
  });

  test('a column that the author aligned keeps its alignment', () => {
    const out = html(['| Name | Plays |', '| --- | :-: |', '| a | 12 |'].join('\n'));
    expect(out).not.toContain('data-num');
    expect(out).toContain('text-align:center');
  });
});
