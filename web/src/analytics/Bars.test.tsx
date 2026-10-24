import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { Bars, niceMax } from './Bars';

const usd = (n: number): string => `$${n.toFixed(2)}`;
const BARS = [
  { label: 'Jan 1', value: 2 },
  { label: 'Jan 2', value: 0 },
  { label: 'Jan 3', value: 5 },
];

describe('Bars', () => {
  test('the_bars_are_focusable_with_their_values', () => {
    const html = renderToStaticMarkup(<Bars metric="Cost" bars={BARS} format={usd} total={7} />);
    expect(html).toContain('role="group"');
    expect(html).toContain('aria-label="Cost by day"');
    expect(html.match(/<rect /g)?.length).toBe(3);
    expect(html.match(/tabindex="0"/g)?.length).toBe(3);
    expect(html).toContain('aria-label="Jan 1: $2.00"');
    expect(html).toContain('aria-label="Jan 3: $5.00"');
    // Without a focus the readout shows the total; the top grid line has the rounded maximum.
    expect(html).toContain('Total · $7.00');
    expect(html).toContain('<span class="fd-bars-top">$5.00</span>');
  });

  test('the_hidden_table_repeats_the_values', () => {
    const html = renderToStaticMarkup(
      <Bars metric="Tokens" bars={BARS} format={usd} total={7} dataFrom={1} by="hour" />,
    );
    const table = html.slice(html.indexOf('<table'), html.indexOf('</table>'));
    expect(table).toContain('class="fd-vh"');
    expect(table).toContain('<caption>Tokens by hour</caption>');
    expect(table).toContain('<th scope="row">Jan 1</th><td>No data</td>');
    expect(table).toContain('<th scope="row">Jan 2</th><td>$0.00</td>');
    expect(table).toContain('<th scope="row">Jan 3</th><td>$5.00</td>');
  });

  test('bars_before_the_first_record_are_not_drawn', () => {
    const since = Date.UTC(2026, 0, 2, 12);
    const html = renderToStaticMarkup(
      <Bars metric="Cost" bars={BARS} format={usd} total={7} dataFrom={1} dataSince={since} />,
    );
    expect(html.match(/<rect /g)?.length).toBe(2);
    expect(html).toContain('class="fd-bars-start"');
    expect(html).toContain('No data before Jan 2');
    const none = renderToStaticMarkup(<Bars metric="Cost" bars={BARS} format={usd} total={7} />);
    expect(none).not.toContain('fd-bars-start');
  });

  test('the_scale_tops_at_a_rounded_maximum', () => {
    expect(niceMax(0)).toBe(1);
    expect(niceMax(0.3)).toBe(0.5);
    expect(niceMax(7)).toBe(10);
    expect(niceMax(120)).toBe(200);
    expect(niceMax(500)).toBe(500);
  });
});
