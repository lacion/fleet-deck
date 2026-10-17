import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import { More } from '../app/More';
import { Sidebar } from '../app/Sidebar';
import type { Route } from '../app/routes';
import { card, station } from '../testing/fleet';
import { PeopleProvider } from './context';
import { env, fakeShareCalls, memberEnv } from './fixtures';
import { GUEST_HIDDEN_ROUTES } from './member';

const owner = env({ shares: fakeShareCalls() });

const app = (
  peopleEnv: ReturnType<typeof env>,
  route: Route,
  layout: 'desktop' | 'phone' = 'desktop',
) =>
  renderToStaticMarkup(
    <PeopleProvider value={peopleEnv}>
      <App
        initialRoute={route}
        initialLayout={layout}
        stations={[station()]}
        sessions={[card('s1')]}
        freshness="live"
        chrome={{
          label: 'Connected',
          freshness: 'live',
          onLogout: () => {},
          notices: [],
        }}
      />
    </PeopleProvider>,
  );

const GUEST_LINKS = [
  '#/activity',
  '#/plans',
  '#/automations',
  '#/artifacts',
  '#/files',
  '#/stations',
];

describe('the owner board (R11)', () => {
  test('the fleet home has "Share board…", the sidebar and More have Shares', () => {
    const html = app(owner, { name: 'fleet' });
    expect(html).toContain('Share board…');
    expect(html).toContain('href="#/shares"');
    expect(html).toContain('New session');
    expect(html).not.toContain('Shared with you');
    const more = app(owner, { name: 'more' }, 'phone');
    expect(more).toContain('href="#/shares"');
  });

  test('a board without share calls (a token or no deck) has no share entries', () => {
    const html = app(env(), { name: 'fleet' });
    expect(html).not.toContain('Share board…');
    expect(html).not.toContain('href="#/shares"');
  });

  test('the Shares and the Spaces pages open for the owner', () => {
    expect(app(owner, { name: 'shares' })).toContain('Loading the shares…');
    expect(app(owner, { name: 'spaces' })).toContain('Loading the spaces…');
  });
});

describe('member mode (R11.4)', () => {
  test('the banner names the role, the owner and the person', () => {
    const html = app(memberEnv('approver'), { name: 'fleet' });
    expect(html).toContain('Shared with you as approver by the owner · Ana');
    expect(html).toContain('>Leave<');
  });

  test('the fleet home has no spawn, no Share board…, no mail tab and no owner links', () => {
    for (const role of ['viewer', 'collaborator', 'approver'] as const) {
      const html = app(memberEnv(role), { name: 'fleet' });
      expect(html).not.toContain('New session');
      expect(html).not.toContain('Share board…');
      expect(html).not.toContain('Mail &amp; tasks');
      for (const href of [...GUEST_LINKS, '#/shares', '#/terminals']) {
        expect(html).not.toContain(`href="${href}"`);
      }
      // The fleet and the watch page stay.
      expect(html).toContain('href="#/fleet"');
      expect(html).toContain('href="#/watch"');
    }
  });

  test('an empty fleet of a guest does not offer "New session"', () => {
    const html = renderToStaticMarkup(
      <PeopleProvider value={memberEnv('viewer')}>
        <App initialRoute={{ name: 'fleet' }} />
      </PeopleProvider>,
    );
    expect(html).toContain('No shared sessions yet.');
    expect(html).not.toContain('Start one with New session');
  });

  test('the sidebar and the More page keep the Watch page only', () => {
    const sidebar = renderToStaticMarkup(
      <PeopleProvider value={memberEnv('viewer')}>
        <Sidebar
          sessions={[]}
          stations={[]}
          route={{ name: 'home' }}
          terminalCount={2}
          onNavigate={() => {}}
          onOpenTerminals={() => {}}
        />
      </PeopleProvider>,
    );
    expect(sidebar).toContain('href="#/watch"');
    expect(sidebar).not.toContain('New session');
    expect(sidebar).not.toContain('Terminals');
    for (const href of [...GUEST_LINKS, '#/shares'])
      expect(sidebar).not.toContain(`href="${href}"`);
    const more = renderToStaticMarkup(
      <PeopleProvider value={memberEnv('viewer')}>
        <More stationCount={1} terminalCount={0} />
      </PeopleProvider>,
    );
    expect(more).toContain('href="#/watch"');
    for (const href of [...GUEST_LINKS, '#/shares', '#/terminals']) {
      expect(more).not.toContain(`href="${href}"`);
    }
  });

  test('the phone bottom bar has no New for a guest', () => {
    expect(app(memberEnv('viewer'), { name: 'fleet' }, 'phone')).not.toContain('href="#/new"');
    expect(app(owner, { name: 'fleet' }, 'phone')).toContain('href="#/new"');
  });

  test('a page that a guest never sees is the home', () => {
    for (const name of GUEST_HIDDEN_ROUTES) {
      const route = (
        name === 'compare'
          ? { name, batch: 'b' }
          : name === 'artifact'
            ? { name, station: 's', id: 'a', version: null }
            : { name }
      ) as Route;
      const html = app(memberEnv('collaborator'), route);
      expect(html).not.toContain('Loading the shares');
      expect(html).not.toContain('aria-label="Spawn');
      expect(html).toContain('aria-label="Fleet board"');
    }
  });

  test('a guest never opens the Shares page, even when the route is forced', () => {
    const html = renderToStaticMarkup(
      <PeopleProvider value={memberEnv('viewer', { shares: fakeShareCalls() })}>
        <App initialRoute={{ name: 'shares' }} />
      </PeopleProvider>,
    );
    expect(html).not.toContain('Loading the shares');
  });
});
