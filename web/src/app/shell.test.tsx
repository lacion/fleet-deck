import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { card, station } from '../testing/fleet';
import { readDensity } from '../ui/density';
import { readTheme } from '../ui/theme';
import { HelpOverlay, shortcuts } from './HelpOverlay';
import { Avatar, PhoneBar, accountName } from './Layout';
import { More, TerminalsEmpty } from './More';
import { PrefsRow } from './Prefs';
import { Sidebar } from './Sidebar';
import { PeopleProvider, ownerEnv } from '../sharing/context';

const chrome = {
  label: 'Connected',
  freshness: 'live' as const,
  onLogout: () => {},
  notices: [],
};

/** The desktop sidebar with a connection: the footer has the account menu. */
const sidebarWithAccount = () =>
  renderToStaticMarkup(
    <Sidebar
      sessions={[card('s1')]}
      stations={[station()]}
      route={{ name: 'fleet' }}
      terminalCount={0}
      chrome={{ ...chrome, addDevice: (() => {}) as never }}
      onNavigate={() => {}}
      onOpenTerminals={() => {}}
    />,
  );

describe('preferences row (DL-141)', () => {
  test('three theme buttons, each with a name and a pressed state; no icon-only toggles', () => {
    const html = renderToStaticMarkup(<PrefsRow />);
    expect(html).toContain('role="group" aria-label="Theme"');
    for (const name of ['Light theme', 'Dark theme', 'System theme']) {
      expect(html).toContain(`aria-label="${name}"`);
      expect(html).toContain(`title="${name}"`);
    }
    expect(html.match(/aria-pressed/g)?.length).toBe(3);
    expect(html).toContain('aria-pressed="true" aria-label="System theme"');
    expect(html).not.toContain('Compact cards');
    expect(html).not.toContain('Reduce motion');
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
    expect(renderToStaticMarkup(<More terminalCount={0} />)).toContain('fd-sidebar-prefs');
  });

  test('the footer keeps settings, the theme track and the avatar; the switches are named', () => {
    const html = sidebarWithAccount();
    const foot = html.slice(html.indexOf('class="fd-sidebar-foot"'));
    const beforeMenu = foot.slice(0, foot.indexOf('<details class="fd-menu"'));
    expect(beforeMenu.match(/class="fd-seg/g)?.length).toBe(1);
    expect(beforeMenu).not.toContain('Compact cards');
    expect(beforeMenu).not.toContain('Reduce motion');
    expect(html).not.toContain('fd-sidebar-density');
    expect(html).not.toContain('fd-sidebar-motion');
  });

  test('the theme icons are 14px in the sidebar and 16px in the touch rows of More', () => {
    const sidebar = renderToStaticMarkup(<PrefsRow />);
    expect(sidebar.match(/width="14"/g)?.length).toBe(3);
    const more = renderToStaticMarkup(<More terminalCount={0} />);
    const prefs = more.slice(more.indexOf('class="fd-sidebar-prefs"'));
    expect(prefs.slice(0, prefs.indexOf('</div></div>')).match(/width="16"/g)?.length).toBe(3);
  });

  test('the More page has a Compact cards switch in place of the toggle', () => {
    const html = renderToStaticMarkup(<More terminalCount={0} />);
    expect(html).not.toContain('fd-sidebar-density');
    expect(html).toMatch(
      /<label class="fd-more-row fd-more-switch">.*Compact cards<\/span><input type="checkbox" role="switch" class="fd-switch" aria-checked="false"/,
    );
  });

  test('Compact cards and Reduce motion: switches in the account menu and on More (WCAG 2.2.2)', () => {
    const menu = sidebarWithAccount().slice(sidebarWithAccount().indexOf('class="fd-menu-body"'));
    for (const name of ['Compact cards', 'Reduce motion']) {
      expect(menu).toMatch(
        new RegExp(
          `<label class="fd-menu-switch"><span class="fd-menu-switch-label"><svg[^>]*width="14".*?${name}</span><input type="checkbox" role="switch" class="fd-switch" aria-checked="false"`,
        ),
      );
    }
    const html = renderToStaticMarkup(<More terminalCount={0} />);
    expect(html).not.toContain('fd-menu-switch');
    expect(html).toMatch(
      /<label class="fd-more-row fd-more-switch">.*Reduce motion<\/span><input type="checkbox" role="switch" class="fd-switch" aria-checked="false"/,
    );
  });

  test('the account menu: who is signed in, a rule, the switches, Add a device, a rule, Log out', () => {
    const html = sidebarWithAccount();
    const menu = html.slice(html.indexOf('class="fd-menu-body"'));
    const at = (text: string) => menu.indexOf(text);
    expect(menu).toStartWith(
      'class="fd-menu-body"><p class="fd-menu-text fd-account-who">Signed in as <span class="fd-account-name">owner</span></p><div class="fd-menu-sep"></div>',
    );
    expect(at('Compact cards')).toBeLessThan(at('Reduce motion'));
    expect(at('Reduce motion')).toBeLessThan(at('Add a device'));
    const rest = menu.slice(at('Add a device'));
    expect(rest.indexOf('<div class="fd-menu-sep"></div>')).toBeLessThan(rest.indexOf('Log out'));
    expect(menu.match(/class="fd-menu-sep"/g)?.length).toBe(3);
  });
});

describe('the More page (§4.8)', () => {
  test('the pages in the order of the sidebar, then search, settings and help', () => {
    const html = renderToStaticMarkup(
      <More terminalCount={0} onPalette={() => {}} onHelp={() => {}} />,
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
    const html = renderToStaticMarkup(<More terminalCount={0} />);
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
    const more = renderToStaticMarkup(<More terminalCount={0} chrome={chrome} />);
    expect(more).toContain('title="Deck version">deck 0.9.1<');
    const none = { ...chrome, version: undefined };
    expect(renderToStaticMarkup(<More terminalCount={0} chrome={none} />)).not.toContain(
      'Deck version',
    );
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

describe('the account avatar and name (§4.2)', () => {
  test('the owner has a person glyph, not the "O" of the word owner', () => {
    const owner = renderToStaticMarkup(<Avatar />);
    expect(owner).toStartWith('<span class="fd-avatar" aria-hidden="true"><svg');
    expect(owner).not.toContain('>O<');
    expect(accountName(ownerEnv)).toBe('owner');
  });

  test('a member shows the initials of the display name', () => {
    const member = {
      ...ownerEnv,
      self: 'member:ana',
      member: { role: 'viewer' as const, name: 'Ana Ruiz', scope: null },
    };
    expect(
      renderToStaticMarkup(
        <PeopleProvider value={member}>
          <Avatar />
        </PeopleProvider>,
      ),
    ).toBe('<span class="fd-avatar" aria-hidden="true">AR</span>');
    expect(accountName(member)).toBe('Ana Ruiz');
    expect(accountName({ self: 'token:ci-bot', member: null })).toBe('ci-bot');
  });
});

describe('the help keys and copy', () => {
  const keys = (apple: boolean) => shortcuts(apple).map((row) => row.keys);

  test('only the keys of the platform: ⌘ on an Apple system, Ctrl elsewhere', () => {
    expect(keys(true)).toContain('⌘K');
    expect(keys(true)).toContain('⌘↵');
    expect(keys(false)).toContain('Ctrl+K');
    expect(keys(false)).toContain('Ctrl+↵');
    for (const k of [...keys(true), ...keys(false)]) expect(k).not.toContain(' or ');
  });

  test('the concepts and the pages say what the board does, without jargon', () => {
    const html = renderToStaticMarkup(<HelpOverlay onClose={() => {}} />);
    expect(html).toContain(
      'Open in terminal hands the session to the provider&#x27;s own terminal app; Return to board control takes it back.',
    );
    expect(html).not.toContain('TUI');
    expect(html).toContain('Every live session by station, the Needs you panel, usage and mail');
    expect(html).not.toContain(' or <kbd');
  });
});

describe('the phone Terminals page without a terminal (§4.7)', () => {
  test('an empty state: the icon, the sentence and one way to the stations', () => {
    const html = renderToStaticMarkup(<TerminalsEmpty />);
    expect(html).toContain('<h1 class="fd-page-title">Terminals</h1>');
    expect(html).toContain('class="fd-panel fd-emptystate"');
    expect(html).toContain('Open a shell from a station or a session.');
    expect(html).toContain('<a href="#/stations" class="fd-btn fd-primary">Go to Stations</a>');
  });
});

describe('the More rows (§4.8)', () => {
  test('a row that opens a page ends in a chevron; a control row or a dialog row has none', () => {
    const html = renderToStaticMarkup(
      <More terminalCount={0} onPalette={() => {}} onHelp={() => {}} />,
    );
    const links = [...html.matchAll(/<a href="#\/[^"]+">(.*?)<\/a>/g)].map((m) => m[1] ?? '');
    expect(links.length).toBeGreaterThan(5);
    for (const link of links) expect(link).toContain('class="fd-more-chev"');
    const buttons = [...html.matchAll(/<button type="button">(.*?)<\/button>/g)].map(
      (m) => m[1] ?? '',
    );
    for (const button of buttons) expect(button).not.toContain('fd-more-chev');
    expect(html).not.toMatch(/fd-more-switch.*?fd-more-chev.*?<\/label>/);
  });
});

describe('the More Stations row', () => {
  test('no total of machines; a hazard badge names the offline stations', () => {
    const row = (stations: Parameters<typeof More>[0]['stations']) => {
      const html = renderToStaticMarkup(<More stations={stations} terminalCount={0} />);
      const at = html.indexOf('href="#/stations"');
      return html.slice(at, html.indexOf('</a>', at));
    };
    expect(row([station()])).not.toContain('fd-count');
    expect(row([station()])).not.toContain('offline');
    expect(row([station(), station('station-b', { reachable: false })])).toContain(
      'Stations<span class="fd-badge" data-tone="hazard">1 offline</span>',
    );
  });
});
