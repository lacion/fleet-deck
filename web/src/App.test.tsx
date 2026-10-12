import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from './App';
import { card, station } from './testing/fleet';

describe('App', () => {
  test('an empty fleet shows every deck status lane with no sessions', () => {
    const html = renderToStaticMarkup(<App initialRoute={{ name: 'fleet' }} />);
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
    expect(html.match(/No sessions</g)?.length).toBe(8);
  });

  test('cards show the title, station, provider badge, workspace, current tool, usage and model', () => {
    const html = renderToStaticMarkup(
      <App
        initialRoute={{ name: 'fleet' }}
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
    expect(html.match(/No sessions</g)?.length).toBe(6);
    expect(html).not.toContain('Cached · stale');
  });

  test('filter controls contain both station identities and do not depend on provider inventory', () => {
    const html = renderToStaticMarkup(
      <App
        initialRoute={{ name: 'fleet' }}
        stations={[station(), station('station-b')]}
        sessions={[card('s')]}
      />,
    );
    expect(html).toContain('aria-label="Fleet filters"');
    for (const value of ['station-a', 'station-b', 'claude', 'codex', 'unknown'])
      expect(html).toContain(`value="${value}"`);
    expect(html).toContain('1 of 1 sessions');
    expect(html).toContain('Stations · 2');
  });

  test('provider text and workspace paths are escaped rather than treated as HTML', () => {
    const html = renderToStaticMarkup(
      <App
        initialRoute={{ name: 'fleet' }}
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

  test('the desktop home shows the sidebar and the fleet', () => {
    const html = renderToStaticMarkup(
      <App initialLayout="desktop" stations={[station()]} sessions={[card('s1')]} />,
    );
    expect(html).toContain('data-layout="desktop"');
    expect(html).toContain('aria-label="Sessions"');
    expect(html).toContain('>Fleet<');
    expect(html).not.toContain('aria-label="Board"');
  });

  test('the phone home is the session list with the bottom bar', () => {
    const html = renderToStaticMarkup(
      <App initialLayout="phone" stations={[station()]} sessions={[card('s1')]} />,
    );
    expect(html).toContain('data-layout="phone"');
    expect(html).toContain('class="fd-srow"');
    expect(html).toContain('aria-label="Board"');
    expect(html).not.toContain('aria-label="Fleet board"');
  });

  test('the phone more page lists the other pages', () => {
    const html = renderToStaticMarkup(
      <App initialLayout="phone" initialRoute={{ name: 'more' }} stations={[station()]} />,
    );
    expect(html).toContain('aria-label="More"');
    expect(html).toContain('Plans');
    expect(html).toContain('Stations · 1');
    expect(html).toContain('aria-current="page">More<');
  });

  test('desktop shows the home for the phone-only more route', () => {
    const html = renderToStaticMarkup(
      <App initialLayout="desktop" initialRoute={{ name: 'more' }} />,
    );
    expect(html).not.toContain('aria-label="More"');
    expect(html).toContain('aria-label="Fleet board"');
  });

  test('a session that is not on the board says so', () => {
    const html = renderToStaticMarkup(
      <App
        initialRoute={{ name: 'session', station: 'station-a', session: 'gone', tab: 'chat' }}
        sessions={[card('s1')]}
      />,
    );
    expect(html).toContain('This session is not on the board.');
  });

  test('the sidebar lists needs-you, one group per station and the ended count', () => {
    const html = renderToStaticMarkup(
      <App
        initialLayout="desktop"
        stations={[station(), station('station-b')]}
        sessions={[
          card('n1', 'needs_you', 'station-b', { title: 'Needs me' }),
          card('w1', 'working', 'station-a', { title: 'Busy' }),
          card('e1', 'ended', 'station-a', { title: 'Done' }),
        ]}
      />,
    );
    expect(html).toContain('Needs you · 1');
    expect(html).toContain('Ended · 1');
    const needs = html.indexOf('Needs you · 1');
    const alpha = html.indexOf('Station Alpha');
    const beta = html.indexOf('Station Beta');
    expect(needs).toBeGreaterThan(-1);
    expect(needs).toBeLessThan(alpha);
    expect(alpha).toBeLessThan(beta);
    // a needs-you session is listed again in its own station
    expect(html.match(/Needs me/g)?.length).toBeGreaterThanOrEqual(2);
  });

  test('an empty fleet says so in the sidebar', () => {
    const html = renderToStaticMarkup(<App initialLayout="desktop" />);
    expect(html).toContain('No sessions yet.');
    expect(html).toContain('New session');
  });

  test('the chrome shows the connection label, log out and the notices', () => {
    const html = renderToStaticMarkup(
      <App
        initialLayout="desktop"
        chrome={{
          label: 'Connected · Live fleet',
          freshness: 'live',
          onLogout: () => {},
          notices: [{ text: 'Cached data is read-only until the deck reconnects.', tone: 'info' }],
        }}
      />,
    );
    expect(html).toContain('Connected · Live fleet');
    expect(html).toContain('Log out');
    expect(html).toContain('Cached data is read-only until the deck reconnects.');
  });
});
