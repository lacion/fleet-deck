import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { card, station } from '../testing/fleet';
import { readDensity } from '../ui/density';
import { readTheme } from '../ui/theme';
import { HelpOverlay } from './HelpOverlay';
import { More } from './More';
import { PrefsRow } from './Prefs';
import { Sidebar } from './Sidebar';

describe('preferences row (DL-141)', () => {
  test('three theme buttons and a compact switch, each with a name and a pressed state', () => {
    const html = renderToStaticMarkup(<PrefsRow />);
    expect(html).toContain('role="group" aria-label="Theme"');
    for (const name of ['Light theme', 'Dark theme', 'System theme', 'Compact cards']) {
      expect(html).toContain(`aria-label="${name}"`);
      expect(html).toContain(`title="${name}"`);
    }
    expect(html.match(/aria-pressed/g)?.length).toBe(4);
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

  test('an ended group offers Clear for its station', () => {
    const html = render();
    expect(html).toContain('aria-label="Archive the ended sessions of Alpha"');
    expect(html).not.toContain('fd-sidebar-archived');
    expect(html).not.toContain('fd-srow-star');
  });
});
