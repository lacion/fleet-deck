import { afterEach, describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { canGoBack, noteNavigation, resetBackNav, startBackNav } from './backNav';
import { Layout } from './Layout';
import { More } from './More';
import type { Route } from './routes';
import type { Layout as LayoutName } from './viewport';

const ROUTES: readonly Route[] = [
  { name: 'artifacts' },
  { name: 'artifact', station: 'st', id: 'a1', version: null },
  { name: 'settings', tab: 'deck' },
  { name: 'automations' },
  { name: 'projects' },
  { name: 'project', id: 'p1' },
  { name: 'compare', batch: 'b1' },
];

function page(layout: LayoutName, route: Route): string {
  return renderToStaticMarkup(
    <Layout
      layout={layout}
      route={route}
      sidebar={null}
      drawerOpen={false}
      onDrawer={() => {}}
      notices={[]}
      dock={null}
      onTerminalsBack={() => {}}
      overlays={null}
    >
      <p>page</p>
    </Layout>,
  );
}

describe('the phone bottom bar (more routes)', () => {
  for (const route of ROUTES) {
    test(`${route.name} shows the bar and highlights More`, () => {
      const html = page('phone', route);
      expect(html).toContain('aria-label="Board"');
      expect(html).toMatch(/<a href="#\/more" aria-current="page">/);
      expect(html).not.toMatch(/<a href="#\/" aria-current="page">/);
    });
  }

  test('the session and terminals routes keep no bar; the desktop never has one', () => {
    const session: Route = { name: 'session', station: 'st', session: 's1', tab: 'chat' };
    expect(page('phone', session)).not.toContain('aria-label="Board"');
    expect(page('phone', { name: 'terminals' })).not.toContain('aria-label="Board"');
    expect(page('desktop', ROUTES[0] as Route)).not.toContain('aria-label="Board"');
  });
});

describe('the phone Terminals page', () => {
  test('has the phone bar of §4.8 with a Back button, the title, Live and the avatar', () => {
    const html = page('phone', { name: 'terminals' });
    expect(html).toContain('<header class="fd-phonebar">');
    expect(html).toContain(
      '<button type="button" class="fd-iconbtn fd-phonebar-back" aria-label="Back">',
    );
    expect(html).toContain('<h1 class="fd-phonebar-title fd-wide">Terminals</h1>');
    expect(html).toContain('class="fd-wspill"');
    expect(html).toContain('class="fd-avatar"');
    expect(html).not.toContain('fd-topbar');
  });
});

describe('the More page search and help buttons', () => {
  test('each button needs its prop', () => {
    const none = renderToStaticMarkup(<More stationCount={0} terminalCount={0} />);
    expect(none).not.toContain('Search sessions');
    expect(none).not.toContain('>Help<');
    const both = renderToStaticMarkup(
      <More stationCount={0} terminalCount={0} onPalette={() => {}} onHelp={() => {}} />,
    );
    expect(both).toContain('Search sessions');
    expect(both).toContain('Help');
    const onlyHelp = renderToStaticMarkup(
      <More stationCount={0} terminalCount={0} onHelp={() => {}} />,
    );
    expect(onlyHelp).not.toContain('Search sessions');
    expect(onlyHelp).toContain('Help');
  });
});

/** A history with a stack of entries, like a browser: push, replace, back and forward. */
function fakeHistory() {
  const entries: unknown[] = [null];
  let at = 0;
  return {
    get state(): unknown {
      return entries[at];
    },
    replaceState(state: unknown, _title: string) {
      entries[at] = state;
    },
    push() {
      entries.splice(at + 1);
      entries.push(null);
      at += 1;
    },
    go(delta: number) {
      at += delta;
    },
  };
}

describe('back navigation position', () => {
  afterEach(resetBackNav);

  test('the first entry is position 0, each new entry is one more', () => {
    const h = fakeHistory();
    startBackNav(h);
    expect(h.state).toEqual({ fdPos: 0 });
    expect(canGoBack(h)).toBe(false);
    h.push();
    noteNavigation(h);
    expect(h.state).toEqual({ fdPos: 1 });
    expect(canGoBack(h)).toBe(true);
    h.push();
    noteNavigation(h);
    expect(h.state).toEqual({ fdPos: 2 });
  });

  test('browser Back to the first entry cannot go back; Forward can again', () => {
    const h = fakeHistory();
    startBackNav(h);
    h.push();
    noteNavigation(h);
    h.go(-1);
    noteNavigation(h);
    expect(canGoBack(h)).toBe(false);
    h.go(1);
    noteNavigation(h);
    expect(canGoBack(h)).toBe(true);
    expect(h.state).toEqual({ fdPos: 1 });
  });

  test('a new entry after Back takes the position of the entry that it follows', () => {
    const h = fakeHistory();
    startBackNav(h);
    h.push();
    noteNavigation(h);
    h.push();
    noteNavigation(h);
    h.go(-1);
    noteNavigation(h);
    h.push();
    noteNavigation(h);
    expect(h.state).toEqual({ fdPos: 2 });
  });

  test('a reload starts at position 0 and keeps other state keys', () => {
    const h = fakeHistory();
    h.replaceState({ other: 'kept', fdPos: 3 }, '');
    startBackNav(h);
    expect(h.state).toEqual({ other: 'kept', fdPos: 0 });
    expect(canGoBack(h)).toBe(false);
  });

  test('an entry without a position cannot go back', () => {
    const h = fakeHistory();
    expect(canGoBack(h)).toBe(false);
    h.replaceState('text', '');
    expect(canGoBack(h)).toBe(false);
  });
});
