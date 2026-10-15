import { afterEach, describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { card, station } from '../testing/fleet';
import type { Route } from './routes';
import { Sidebar } from './Sidebar';

const store = (items: Record<string, string>) => {
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: { getItem: (k: string) => items[k] ?? null },
  });
};

afterEach(() => {
  Reflect.deleteProperty(globalThis, 'localStorage');
});

const chrome = {
  label: 'Connected',
  freshness: 'live' as const,
  onLogout: () => {},
  notices: [],
};

const render = (
  route: Route = { name: 'fleet' },
  extra: { terminalCount?: number; chrome?: typeof chrome & { addDevice?: () => never } } = {},
  sessions = [card('s1')],
) =>
  renderToStaticMarkup(
    <Sidebar
      sessions={sessions}
      stations={[station()]}
      route={route}
      terminalCount={extra.terminalCount ?? 0}
      chrome={extra.chrome ?? chrome}
      onNavigate={() => {}}
      onOpenTerminals={() => {}}
    />,
  );

const labels = (html: string, from: string, to: string): string[] => {
  const part = html.slice(html.indexOf(from), html.indexOf(to));
  return [...part.matchAll(/<a href="#\/[^"]*"[^>]*>(?:<svg.*?<\/svg>)?([A-Za-z]+)/g)].map(
    (m) => m[1] ?? '',
  );
};

describe('the sidebar navigation (S1)', () => {
  test('the links come before the session list, the footer after it', () => {
    const html = render();
    const nav = html.indexOf('aria-label="Board pages"');
    const list = html.indexOf('class="fd-sidebar-list"');
    const foot = html.indexOf('class="fd-sidebar-foot"');
    expect(nav).toBeGreaterThan(html.indexOf('class="fd-sidebar-top"'));
    expect(nav).toBeLessThan(list);
    expect(list).toBeLessThan(foot);
    expect(html).not.toContain('fd-sidebar-bottom');
  });

  test('four main links, then the More fold with the other pages', () => {
    const html = render({ name: 'fleet' }, { terminalCount: 2 });
    const main = labels(html, 'aria-label="Board pages"', '<details class="fd-sidebar-more"');
    expect(main).toEqual(['Fleet', 'Activity', 'Stations', 'Watch']);
    const more = html.slice(
      html.indexOf('<details class="fd-sidebar-more"'),
      html.indexOf('</nav>'),
    );
    expect(more).toContain('<summary><span>More</span>');
    expect(labels(more, '<summary>', '</details>')).toEqual([
      'Plans',
      'Automations',
      'Artifacts',
      'Files',
    ]);
    expect(more).toContain('Terminals');
    expect(more).not.toContain('Settings');
  });

  test('the More fold is closed on a main page and opens for a page inside it', () => {
    expect(render({ name: 'fleet' })).toContain('<details class="fd-sidebar-more">');
    const files = render({ name: 'files' });
    expect(files).toContain('<details class="fd-sidebar-more" open="">');
    expect(files).toContain('href="#/files" aria-current="page"');
    expect(render({ name: 'plans' })).toContain('<details class="fd-sidebar-more" open="">');
  });

  test('the browser keeps the last state of the More fold', () => {
    store({ 'fd-sidebar-more': '1' });
    expect(render({ name: 'fleet' })).toContain('<details class="fd-sidebar-more" open="">');
    store({ 'fd-sidebar-more': '0' });
    expect(render({ name: 'fleet' })).toContain('<details class="fd-sidebar-more">');
    // Blocked storage: the fold is closed.
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      get() {
        throw new Error('blocked');
      },
    });
    expect(render({ name: 'fleet' })).toContain('<details class="fd-sidebar-more">');
  });

  test('the footer has the preferences, and the Account menu has the two actions', () => {
    const html = render(
      { name: 'fleet' },
      { chrome: { ...chrome, addDevice: (() => {}) as never } },
    );
    const foot = html.slice(html.indexOf('class="fd-sidebar-foot"'));
    expect(foot.indexOf('fd-sidebar-prefs')).toBeLessThan(foot.indexOf('fd-sidebar-account'));
    const menu = foot.slice(foot.indexOf('<details class="fd-menu"'));
    expect(menu).toContain('<summary class="fd-iconbtn" aria-label="Account">');
    expect(menu).toContain('Add a device');
    expect(menu).toContain('Log out');
    // Outside the menu, the footer has neither action.
    expect(foot.slice(0, foot.indexOf('<details class="fd-menu"'))).not.toContain('Log out');
    // Without an add-device provider the menu has Log out only.
    const plain = render();
    expect(plain).toContain('Log out');
    expect(plain).not.toContain('Add a device');
  });

  test('a session row has no middle dot between the model, the branch and the folder', () => {
    const html = render({ name: 'fleet' }, {}, [
      card('b1', 'working', 'station-a', { branch: 'feat/x', cwd: '/work/app' }),
      card('f1', 'working', 'station-a', { branch: null, cwd: '/work/app' }),
    ]);
    const metas = [...html.matchAll(/<span class="fd-srow-meta">(.*?)<\/a>/g)].map((m) => m[1]);
    expect(metas.length).toBe(2);
    for (const meta of metas) {
      expect(meta).not.toContain('·');
      expect(meta).toContain('<span class="fd-srow-model">');
    }
    expect(metas[0]).toContain('feat/x');
    expect(metas[1]).toContain('app');
  });
});
