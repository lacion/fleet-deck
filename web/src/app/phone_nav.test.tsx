import { afterEach, describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { canGoBack, noteNavigation, resetBackNav } from './backNav';
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

describe('back navigation counter', () => {
  afterEach(resetBackNav);

  test('starts at zero, counts navigations and resets', () => {
    expect(canGoBack()).toBe(false);
    noteNavigation();
    expect(canGoBack()).toBe(true);
    noteNavigation();
    expect(canGoBack()).toBe(true);
    resetBackNav();
    expect(canGoBack()).toBe(false);
  });
});
