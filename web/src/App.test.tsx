import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from './App';
import { summary } from './testing/summary';

describe('App', () => {
  test('an empty fleet shows every lane with no sessions', () => {
    const html = renderToStaticMarkup(<App />);
    expect(html).toContain('FLEET DECK');
    for (const label of ['Needs you', 'Working', 'Background', 'Idle', 'Ended']) {
      expect(html).toContain(`aria-label="${label}"`);
    }
    expect(html.match(/No sessions/g)?.length).toBe(5);
  });

  test('cards show the title, the provider badge and the model', () => {
    const html = renderToStaticMarkup(
      <App
        sessions={[
          summary('s1', 'needs_you', 1, { title: 'fix login', model: 'opus' }),
          summary('s2', 'working', 2, { provider: 'codex' }),
        ]}
      />,
    );
    expect(html).toContain('fix login');
    expect(html).toContain('data-provider="claude"');
    expect(html).toContain('>Codex<');
    expect(html).toContain('opus');
    expect(html.match(/No sessions/g)?.length).toBe(3);
  });
});
