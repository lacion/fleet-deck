import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from './App';
import { card, station } from './testing/fleet';

describe('App', () => {
  test('an empty fleet shows every deck status lane with no sessions', () => {
    const html = renderToStaticMarkup(<App />);
    expect(html).toContain('FLEET DECK');
    for (const label of [
      'Needs you',
      'Queued',
      'Working',
      'Verifying',
      'Idle',
      'Ended',
      'Unreachable',
      'Lost',
    ]) {
      expect(html).toContain(`aria-label="${label}"`);
    }
    expect(html.match(/No sessions/g)?.length).toBe(8);
  });

  test('cards show the title, station, provider badge, workspace, current tool, usage and model', () => {
    const html = renderToStaticMarkup(
      <App
        stations={[station()]}
        freshness="live"
        sessions={[
          card('s1', 'needs_you', 'station-a', {
            title: 'fix login',
            model: 'opus',
            cwd: '/repos/deck',
            provider_instance: 'personal',
            running_tools: [
              {
                node_id: 'tool1',
                tool_kind: 'shell',
                title: 'Run tests',
                input_summary: 'cargo test',
              },
            ],
            usage: { input_tokens: 12, output_tokens: 0, cost_usd: 0 },
          }),
          card('s2', 'working', 'station-a', { provider: 'codex' }),
        ]}
      />,
    );
    for (const text of [
      'fix login',
      'data-provider="claude"',
      '>Codex<',
      'opus',
      'Alpha',
      '/repos/deck',
      'cargo test',
      'Instance: personal',
      'Input 12',
      'Output 0',
      '$0.0000',
    ])
      expect(html).toContain(text);
    expect(html.match(/No sessions/g)?.length).toBe(6);
    expect(html).not.toContain('Cached · stale');
  });

  test('filter controls contain both station identities and do not depend on provider inventory', () => {
    const html = renderToStaticMarkup(
      <App stations={[station(), station('station-b')]} sessions={[card('s')]} />,
    );
    expect(html).toContain('aria-label="Fleet filters"');
    for (const value of ['station-a', 'station-b', 'claude', 'codex', 'unknown'])
      expect(html).toContain(`value="${value}"`);
    expect(html).toContain('1 of 1 sessions');
    expect(html).toContain('Stations (2)');
  });

  test('provider text and workspace paths are escaped rather than treated as HTML', () => {
    const html = renderToStaticMarkup(
      <App
        sessions={[
          card('s', 'working', 'station-a', {
            title: '<script>bad()</script>',
            cwd: '<img src=x>',
          }),
        ]}
      />,
    );
    expect(html).not.toContain('<script>');
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;script&gt;');
  });
});
