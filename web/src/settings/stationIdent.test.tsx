// The head of a station tab and the tab row of the settings page (live UI audit round 2): the
// build without its stamp, the reach in words only when it is offline, whole tabs at the edges.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { StationView } from '../gen';
import { traitsText } from '../board/StationTraits';
import { station } from '../testing/fleet';
import { snapLeft, StationIdent, versionText } from './SettingsPage';

const TRAITS = { os: 'linux', arch: 'x86_64', labels: ['gpu'] };

const info = (version: string) => ({ version, traits: TRAITS }) as unknown as StationView['info'];

describe('the head of a station tab', () => {
  test('the release without the date stamp and the commit', () => {
    expect(versionText('2.0.0-dev.20261022+4b29e272ae')).toBe('2.0.0-dev');
    expect(versionText('2.0.0+4b29e272ae')).toBe('2.0.0');
    expect(versionText('2.1.0-rc.1')).toBe('2.1.0-rc.1');
    expect(versionText('2.0.0-20261022')).toBe('2.0.0');
    expect(versionText('2.0.0')).toBe('2.0.0');
  });

  test('online: the glyph and the name, "online" for screen readers only; the build line', () => {
    const html = renderToStaticMarkup(
      <StationIdent station={station('station-a', { info: info('2.0.0-dev.20261022+4b29e') })} />,
    );
    expect(html).toContain('<span class="fd-dot" data-status="online" aria-hidden="true"></span>');
    expect(html).toContain('<span class="fd-wide fd-set-ident-name">Alpha</span>');
    expect(html).toContain('<span class="fd-vh"> online</span>');
    expect(html).not.toContain('fd-set-ident-off');
    // A line breaks only after a middle dot: a no-break space sits before each one.
    expect(html).toContain(traitsText(TRAITS).replaceAll(' · ', '\u00a0· '));
    expect(html).toContain('Linux\u00a0· x86_64\u00a0·');
    expect(html).toContain(
      '<p class="fd-set-ident-version" title="Fleet Deck 2.0.0-dev.20261022+4b29e">Fleet Deck 2.0.0-dev</p>',
    );
  });

  test('offline: the word in the hazard ink and the note', () => {
    const html = renderToStaticMarkup(
      <StationIdent station={station('station-a', { reachable: false })} />,
    );
    expect(html).toContain('data-status="unreachable"');
    expect(html).toContain('<span class="fd-set-ident-off">offline</span>');
    expect(html).toContain('The settings show and save while the station is online.');
    expect(html).not.toContain('fd-set-ident-version');
  });
});

describe('the tab row stops on whole tabs', () => {
  // Five tabs of 60px, 16px apart, from 16px (the phone padding); a 200px view; 16px of padding.
  const starts = [16, 92, 168, 244, 320];
  const view = (left: number) => ({ left, width: 200, max: 196 });
  const tab = (i: number) => ({ start: starts[i] ?? 0, end: (starts[i] ?? 0) + 60 });

  test('a tab in view, clear of the end fade, keeps the row still', () => {
    expect(snapLeft(starts, tab(1), view(0), 16)).toBe(0);
  });

  test('a tab past the end: the first tab start from which it ends clear of the fade', () => {
    // Tab 3 ends at 304; with the 32px fade it needs 336 - 200 = 136: the start of tab 2 (152).
    expect(snapLeft(starts, tab(3), view(0), 16)).toBe(152);
    expect(snapLeft(starts, tab(4), view(0), 16)).toBe(196);
  });

  test('a tab before the start: it starts the row, one padding in', () => {
    expect(snapLeft(starts, tab(1), view(152), 16)).toBe(76);
    expect(snapLeft(starts, tab(0), view(152), 16)).toBe(0);
  });
});
