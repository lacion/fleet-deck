import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { HELP_SEEN_KEY, HelpNotice, helpNoticeDue, markHelpSeen } from './HelpNotice';

const memory = (seen = false) => {
  const data = new Map<string, string>(seen ? [[HELP_SEEN_KEY, '1']] : []);
  return {
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, v),
  };
};

describe('the help notice (DL-171)', () => {
  test('it is due on a live board of a new browser, and only once', () => {
    const store = memory();
    expect(helpNoticeDue(true, store)).toBe(true);
    markHelpSeen(store);
    expect(helpNoticeDue(true, store)).toBe(false);
  });

  test('it is not due before the board is live or without storage', () => {
    expect(helpNoticeDue(false, memory())).toBe(false);
    expect(helpNoticeDue(true, null)).toBe(false);
    expect(helpNoticeDue(true, memory(true))).toBe(false);
  });

  test('a blocked storage does not throw', () => {
    const blocked = {
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      },
    };
    expect(helpNoticeDue(true, blocked)).toBe(false);
    expect(() => markHelpSeen(blocked)).not.toThrow();
  });

  test('the notice text and a dismiss button', () => {
    const html = renderToStaticMarkup(<HelpNotice onDismiss={() => {}} />);
    expect(html).toContain('Press ? for the keys and a short guide.');
    expect(html).toContain('aria-label="Dismiss the help notice"');
    expect(html).not.toContain('role="dialog"');
  });
});
