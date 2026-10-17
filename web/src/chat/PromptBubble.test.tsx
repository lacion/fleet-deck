import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FOLD_CHARS, FOLD_LINES, foldedText, PromptBubble } from './PromptBubble';

const lines = (n: number) => Array.from({ length: n }, (_, i) => `line ${i + 1}`).join('\n');
const bubble = (text: string, initialOpen = false) =>
  renderToStaticMarkup(<PromptBubble text={text} initialOpen={initialOpen} />);

describe('PromptBubble (backlog B10)', () => {
  test('a short prompt shows as it is, with no button', () => {
    const html = bubble('one\n\ntwo');
    expect(html).toBe('<div class="fd-chat-bubble">one\n\ntwo</div>');
  });

  test('a prompt of exactly 12 lines and 1200 characters is not folded', () => {
    expect(foldedText(lines(FOLD_LINES))).toBeNull();
    expect(foldedText('x'.repeat(FOLD_CHARS))).toBeNull();
    expect(foldedText(lines(FOLD_LINES + 1))).not.toBeNull();
    expect(foldedText('x'.repeat(FOLD_CHARS + 1))).toHaveLength(FOLD_CHARS);
  });

  test('a 30-line prompt is folded to 12 lines with Show more, and opens fully', () => {
    const text = lines(30);
    const folded = bubble(text);
    expect(folded).toContain('line 12');
    expect(folded).not.toContain('line 13');
    expect(folded).toContain('aria-expanded="false"');
    expect(folded).toContain('>Show more</button>');
    const open = bubble(text, true);
    expect(open).toContain('line 30');
    expect(open).toContain('aria-expanded="true"');
    expect(open).toContain('>Show less</button>');
    expect(open).not.toContain('Show more');
  });

  test('a long single line is cut to 1200 characters while folded', () => {
    const html = bubble('y'.repeat(3000));
    expect(html).toContain('y'.repeat(FOLD_CHARS));
    expect(html).not.toContain('y'.repeat(FOLD_CHARS + 1));
    expect(html).toContain('Show more');
  });

  test('a text that ends with the cut mark shows the note only when open', () => {
    const text = `${lines(30)}…`;
    expect(bubble(text)).not.toContain('The station keeps');
    expect(bubble(text, true)).toContain(
      '<p class="fd-chat-cut">The station keeps the first 32 KB of a prompt in the chat.</p>',
    );
    expect(bubble(lines(30), true)).not.toContain('The station keeps');
  });
});
