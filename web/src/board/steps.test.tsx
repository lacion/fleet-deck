import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { Steps } from './Steps';

const LABELS = ['Choose', 'Run', 'Confirm'];
const marks = (html: string) =>
  [...html.matchAll(/class="fd-steps-mark"[^>]*>([^<]*)</g)].map((m) => m[1]);

describe('Steps', () => {
  test('the current step has aria-current and the earlier steps show a check', () => {
    const html = renderToStaticMarkup(<Steps labels={LABELS} current={1} />);
    expect(html.match(/aria-current="step"/g)).toHaveLength(1);
    expect(html).toMatch(/aria-current="step"(?:(?!<\/li>).)*Run/);
    expect(marks(html)).toEqual(['✓', '2', '3']);
    expect(html).toContain('Choose');
    expect(html).toContain('Confirm');
  });

  test('the first step shows no check', () => {
    const html = renderToStaticMarkup(<Steps labels={LABELS} current={0} />);
    expect(marks(html)).toEqual(['1', '2', '3']);
  });

  test('done shows a check on every step and no current step', () => {
    const html = renderToStaticMarkup(<Steps labels={LABELS} current={2} done />);
    expect(marks(html)).toEqual(['✓', '✓', '✓']);
    expect(html).not.toContain('aria-current');
  });
});
