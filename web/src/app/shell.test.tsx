import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { card, station } from '../testing/fleet';
import { readDensity } from '../ui/density';
import { readTheme } from '../ui/theme';
import { HelpOverlay } from './HelpOverlay';
import { PhoneBar } from './Layout';
import { More } from './More';
import { PrefsRow } from './Prefs';
import { Sidebar } from './Sidebar';

describe('preferences row (DL-141)', () => {
  test('three theme buttons and a compact switch, each with a name and a pressed state', () => {
    const html = renderToStaticMarkup(<PrefsRow />);
    expect(html).toContain('role="group" aria-label="Theme"');
    for (const name of [
      'Light theme',
      'Dark theme',
      'System theme',
      'Compact cards',
      'Reduce motion',
    ]) {
      expect(html).toContain(`aria-label="${name}"`);
      expect(html).toContain(`title="${name}"`);
    }
    expect(html.match(/aria-pressed/g)?.length).toBe(5);
    expect(html).toContain('aria-pressed="true" aria-label="System theme"');
  });

  test('the choices default to system and cozy without a browser', () => {
    expect(readTheme()).toBe('system');
    expect(readDensity()).toBe('cozy');
  });

  test('the sidebar footer and the More page carry the row', () => {
    const sidebar = renderToStaticMarkup(
      <Sidebar
        sessions={[]}
        stations={[]}
        route={{ name: 'home' }}
        terminalCount={0}
        onNavigate={() => {}}
        onOpenTerminals={() => {}}
      />,
    );
    expect(sidebar).toContain('fd-sidebar-prefs');
    expect(renderToStaticMarkup(<More stationCount={0} terminalCount={0} />)).toContain(
      'fd-sidebar-prefs',
    );
  });

  test('compact cards is a plain icon toggle, outside the theme track', () => {
    const html = renderToStaticMarkup(<PrefsRow />);
    expect(html).toContain(
      'class="fd-iconbtn fd-sm fd-sidebar-density" aria-pressed="false" aria-label="Compact cards"',
    );
    expect(html.match(/class="fd-seg/g)?.length).toBe(1);
    expect(renderToStaticMarkup(<PrefsRow density={false} />)).not.toContain('Compact cards');
  });

  test('the theme icons are 14px in the sidebar and 16px in the touch rows of More', () => {
    const sidebar = renderToStaticMarkup(<PrefsRow />);
    expect(sidebar.match(/width="14"/g)?.length).toBe(5);
    const more = renderToStaticMarkup(<More stationCount={0} terminalCount={0} />);
    const prefs = more.slice(more.indexOf('class="fd-sidebar-prefs"'));
    expect(prefs.slice(0, prefs.indexOf('</div></div>')).match(/width="16"/g)?.length).toBe(3);
  });

  test('the More page has a Compact cards switch in place of the toggle', () => {
    const html = renderToStaticMarkup(<More stationCount={0} terminalCount={0} />);
    expect(html).not.toContain('fd-sidebar-density');
    expect(html).toMatch(
      /<label class="fd-more-row fd-more-switch">.*Compact cards<\/span><input type="checkbox" role="switch" class="fd-switch" aria-checked="false"/,
    );
  });

  test('Reduce motion: a toggle in the sidebar row, a switch on the More page (WCAG 2.2.2)', () => {
    expect(renderToStaticMarkup(<PrefsRow />)).toContain(
      'class="fd-iconbtn fd-sm fd-sidebar-motion" aria-pressed="false" aria-label="Reduce motion"',
    );
    const html = renderToStaticMarkup(<More stationCount={0} terminalCount={0} />);
    expect(html).not.toContain('fd-sidebar-motion');
    expect(html).toMatch(
      /<label class="fd-more-row fd-more-switch">.*Reduce motion<\/span><input type="checkbox" role="switch" class="fd-switch" aria-checked="false"/,
    );
  });
});

describe('the More page (§4.8)', () => {
  test('the pages in the order of the sidebar, then search, settings and help', () => {
    const html = renderToStaticMarkup(
      <More stationCount={2} terminalCount={0} onPalette={() => {}} onHelp={() => {}} />,
    );
    const nav = html.slice(html.indexOf('aria-label="More pages"'), html.indexOf('</nav>'));
    const lists = nav.split('<ul class="fd-more-links">').slice(1);
    expect(lists).toHaveLength(2);
    const order = (list: string) => [...list.matchAll(/<\/svg>([A-Z][a-z ]+)/g)].map((m) => m[1]);
    expect(order(lists[0] ?? '')).toEqual([
      'Watch',
      'Activity',
      'Stations',
      'Plans',
      'Automations',
      'Artifacts',
      'Files',
      'Terminals',
    ]);
    expect(order(lists[1] ?? '')).toEqual(['Search sessions', 'Help']);
  });

  test('without search, settings and help there is no second group', () => {
    const html = renderToStaticMarkup(<More stationCount={0} terminalCount={0} />);
    const nav = html.slice(html.indexOf('aria-label="More pages"'), html.indexOf('</nav>'));
    expect(nav.split('<ul class="fd-more-links">')).toHaveLength(2);
  });
});

describe('deck version (DL-142)', () => {
  const chrome = {
    label: 'Connected · Live fleet',
    freshness: 'live' as const,
    onLogout: () => {},
    notices: [],
    version: '0.9.1',
  };
  test('the sidebar and More show the version when it is known', () => {
    const sidebar = renderToStaticMarkup(
      <Sidebar
        sessions={[]}
        stations={[]}
        route={{ name: 'home' }}
        terminalCount={0}
        chrome={chrome}
        onNavigate={() => {}}
        onOpenTerminals={() => {}}
      />,
    );
    expect(sidebar).not.toContain('Deck version');
    const more = renderToStaticMarkup(<More stationCount={0} terminalCount={0} chrome={chrome} />);
    expect(more).toContain('title="Deck version">deck 0.9.1<');
    const none = { ...chrome, version: undefined };
    expect(
      renderToStaticMarkup(<More stationCount={0} terminalCount={0} chrome={none} />),
    ).not.toContain('Deck version');
  });
});

describe('help overlay (DL-141)', () => {
  const html = renderToStaticMarkup(<HelpOverlay onClose={() => {}} />);

  test('keeps the dialog and adds the c key and the fleet-home keys', () => {
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-label="Keyboard and help"');
    expect(html).toContain('Open the mail form');
    expect(html).toContain('On the fleet home');
    expect(html).toContain('Answer with option N');
  });

  test('explains the concepts and the pages', () => {
    for (const text of ['Concepts', 'Pages', 'Needs you', 'Worktree', 'Return to board control'])
      expect(html).toContain(text);
    expect(html).toContain('Start an agent on a station');
  });

  test('the header stays; the body under it scrolls, with the two key groups', () => {
    const head = html.indexOf('class="fd-help-head"');
    const body = html.indexOf('class="fd-help-body"');
    expect(head).toBeGreaterThan(-1);
    expect(body).toBeGreaterThan(head);
    expect(html).toContain('<div class="fd-help-keys"><div class="fd-help-keygroup">');
    expect(html.match(/class="fd-help-keygroup"/g)?.length).toBe(2);
  });

  test('two heading levels: section headings over .fd-label sub-headings', () => {
    for (const name of ['Keys', 'Concepts', 'Pages'])
      expect(html).toContain(`<h3 class="fd-help-section">${name}</h3>`);
    expect(html).toContain('<h4 class="fd-label">On every page</h4>');
    expect(html).toContain('<h4 class="fd-label">On the fleet home</h4>');
    expect(html).not.toContain('<h3 class="fd-label">');
  });
});

describe('the phone top bar (§4.8)', () => {
  const bar = (route: Parameters<typeof PhoneBar>[0]['route']) =>
    renderToStaticMarkup(<PhoneBar route={route} chrome={undefined} />);

  test('a detail page goes back to its list, not to More', () => {
    const html = bar({ name: 'artifact', station: 'station-a', id: 'a1', version: null });
    expect(html).toContain('href="#/artifacts"');
    expect(html).toContain('aria-label="Back to Artifacts"');
    expect(html).not.toContain('Back to More');
  });

  test('a list page of the More tab goes back to More; the More page shows the mark', () => {
    const html = bar({ name: 'stations' });
    expect(html).toContain('href="#/more"');
    expect(html).toContain('aria-label="Back to More"');
    expect(bar({ name: 'more' })).not.toContain('fd-phonebar-back');
  });
});

describe('sidebar stars and archive (DL-141)', () => {
  const render = () =>
    renderToStaticMarkup(
      <Sidebar
        sessions={[card('s1'), card('e1', 'ended')]}
        stations={[station()]}
        route={{ name: 'home' }}
        terminalCount={0}
        onNavigate={() => {}}
        onOpenTerminals={() => {}}
      />,
    );

  test('New session and the search share one row (§4.2)', () => {
    const html = render();
    const row = html.slice(html.indexOf('class="fd-sidebar-actions"'));
    expect(row.indexOf('fd-newbtn')).toBeGreaterThan(-1);
    expect(row.indexOf('fd-newbtn')).toBeLessThan(row.indexOf('fd-sidebar-search'));
    expect(row).toContain('aria-label="Search sessions"');
    expect(row).toContain('<span class="fd-newbtn-text">New session</span>');
  });

  test('an empty phone list names the New of the bottom bar; the desktop does not', () => {
    const empty = (variant: 'full' | 'list') =>
      renderToStaticMarkup(
        <Sidebar
          sessions={[]}
          stations={[]}
          route={{ name: 'home' }}
          variant={variant}
          terminalCount={0}
          onNavigate={() => {}}
          onOpenTerminals={() => {}}
        />,
      );
    expect(empty('list')).toContain('No sessions yet. Start one with New.');
    expect(empty('full')).toContain('No sessions yet.');
    expect(empty('full')).not.toContain('Start one with');
  });

  test('an ended group offers Clear for its station', () => {
    const html = render();
    expect(html).toContain('aria-label="Archive the ended sessions of Alpha"');
    expect(html).not.toContain('fd-sidebar-archived');
    expect(html).not.toContain('fd-srow-star');
  });
});
