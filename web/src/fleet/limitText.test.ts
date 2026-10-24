// B27 §3.7: the display of a grant cap.
import { describe, expect, test } from 'bun:test';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { FleetAnswersDialog } from '../session/FleetAnswersDialog';
import { limitText, NO_LIMIT } from './limitText';

describe('limitText', () => {
  test('the largest u32 is "no limit"; a number shows as the number', () => {
    expect(NO_LIMIT).toBe(4294967295);
    expect(limitText(4294967295)).toBe('no limit');
    expect(limitText(0)).toBe('0');
    expect(limitText(500)).toBe('500');
    expect(limitText(4294967294)).toBe('4294967294');
  });

  test('the answers dialog shows the words for no limit', () => {
    const data = (answer_cap: number) => ({
      fleet_control: true,
      answer_cap,
      answer_used: 3,
      last_hour: 0,
      answers: [],
    });
    const html = (cap: number) =>
      renderToStaticMarkup(
        createElement(FleetAnswersDialog, { data: data(cap), onClose: () => {} }),
      );
    expect(html(NO_LIMIT)).toContain('Answer limit: 3 of no limit used');
    expect(html(7)).toContain('Answer limit: 3 of 7 used');
  });
});
