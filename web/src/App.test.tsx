import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from './App';
import { card, station } from './testing/fleet';

describe('App', () => {
  test('an empty fleet shows the five board columns and says how to start a session', () => {
    const html = renderToStaticMarkup(<App initialRoute={{ name: 'fleet' }} />);
    expect(html).toContain('FLEET DECK');
    expect(html).toContain('aria-label="Fleet board"');
    for (const label of ['Queued', 'Working', 'Verifying', 'Idle', 'Offline']) {
      expect(html).toContain(`<span class="fd-label">${label}</span>`);
    }
    expect(html).not.toContain('aria-label="Station ');
    expect(html).toContain('No sessions yet. Start one with New session.');
  });

  test('a card is a link to its session with the title, branch, model, current tool and cost', () => {
    const html = renderToStaticMarkup(
      <App
        initialRoute={{ name: 'fleet' }}
        stations={[station()]}
        freshness="live"
        sessions={[
          card('s1', 'needs_you', 'station-a', {
            title: 'fix login',
            model: 'claude-opus-4-1-20250805',
            cwd: '/repos/deck',
            branch: 'fd/login',
            provider_instance: 'personal',
            running_tools: [
              {
                node_id: 'tool1',
                tool_kind: 'shell',
                title: 'Run tests',
                input_summary: 'cargo test',
              },
            ],
            usage: { input_tokens: 12, output_tokens: 0, cost_usd: 0, cost_inferred: false },
          }),
          card('s2', 'working', 'station-a'),
        ]}
      />,
    );
    for (const text of [
      'fix login',
      'aria-label="Station Alpha"',
      'class="fd-card"',
      'href="#/s/station-a/s1"',
      'Opus 4.1',
      'fd/login',
      'cargo test',
      '$0.00',
    ])
      expect(html).toContain(text);
    expect(html).not.toContain('Stale');
  });

  test('filter chips name both stations and do not depend on provider inventory', () => {
    const html = renderToStaticMarkup(
      <App
        initialRoute={{ name: 'fleet' }}
        stations={[station(), station('station-b')]}
        sessions={[card('s')]}
      />,
    );
    expect(html).toContain('aria-label="Fleet filters"');
    for (const name of ['Alpha', 'Beta', 'Claude', 'Codex'])
      expect(html).toMatch(new RegExp(`<button[^>]*class="fd-chip"[^>]*>${name}</button>`));
    // no filter is set: there is no "N of M" output
    expect(html).not.toContain('<output');
    expect(html).toContain('2 stations');
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
    expect(html).toMatch(/Stations<span class="fd-count">1<\/span>/);
    expect(html).toMatch(/aria-current="page">(<svg[\s\S]*?<\/svg>)?<span>More<\/span>/);
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

  test('the sidebar lists one group per station, a needs-you chip and the ended count', () => {
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
    // desktop: a chip links to the first session that needs you; no group of its own
    expect(html).toMatch(
      /<a class="fd-needschip" href="#\/s\/station-b\/n1">[\s\S]*?1 needs you<\/a>/,
    );
    expect(html).not.toContain('Needs you · 1');
    expect(html).toContain('Ended · 1');
    const alpha = html.indexOf('aria-label="Station Alpha"');
    const beta = html.indexOf('aria-label="Station Beta"');
    expect(alpha).toBeGreaterThan(-1);
    expect(alpha).toBeLessThan(beta);
    // the sidebar lists the needs-you session once, in its own station
    const sidebar = html.slice(html.indexOf('<aside class="fd-sidebar"'), html.indexOf('</aside>'));
    expect(sidebar.match(/Needs me/g)?.length).toBe(1);
  });

  test('the phone session list keeps the needs-you group before the stations', () => {
    const html = renderToStaticMarkup(
      <App
        initialLayout="phone"
        stations={[station(), station('station-b')]}
        sessions={[
          card('n1', 'needs_you', 'station-b', { title: 'Needs me' }),
          card('w1', 'working', 'station-a', { title: 'Busy' }),
        ]}
      />,
    );
    const needs = html.indexOf('Needs you · 1');
    expect(needs).toBeGreaterThan(-1);
    expect(needs).toBeLessThan(html.indexOf('aria-label="Station Alpha"'));
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
