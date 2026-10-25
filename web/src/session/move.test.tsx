// FG-9 (design §12.4A, §12.8): the move dialog, the banner, "Undo move", the menu item and the
// composer of a moved session.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, Project } from '../gen';
import type { MoveRead, MoveView } from '../integrations/actions';
import { IntegrationsProvider } from '../integrations/context';
import { fakeActions, project } from '../integrations/fixtures';
import type { Freshness } from '../runtime/state';
import { card, station } from '../testing/fleet';
import { sessionActions } from './commands';
import { sessionFlags } from './controller';
import { MoveBanner } from './MoveBanner';
import { MoveDialog } from './MoveDialog';
import { MoveUndo } from './MoveUndo';
import { SessionHeader } from './SessionHeader';
import { SessionView } from './SessionView';

const noop = () => {};
const stations = [station('station-a'), station('station-b')];
const synced = (extra: Partial<Project> = {}): Project =>
  project({ git: { ...project().git, sync: true, default_branch: 'main' }, ...extra });

const session = (extra: Partial<DeckSession['summary']> = {}): DeckSession => ({
  ...card('s1', 'idle', 'station-a', { callsign: 'ada', ...extra }),
  project_id: 'p1',
});

const provide = (node: React.ReactNode, projects = [synced()], canAdmin = true) =>
  renderToStaticMarkup(
    <IntegrationsProvider actions={fakeActions({ canAdmin })} initial={projects}>
      {node}
    </IntegrationsProvider>,
  );

const move = (extra: Partial<MoveView> = {}): MoveView => ({
  move_id: 'm1',
  state: 'holding',
  kind: 'native',
  src_station: 'station-a',
  src_session: 's1',
  dst_station: 'station-b',
  created_at: 1,
  ...extra,
});

describe('the move dialog', () => {
  const read: MoveRead = {
    move: null,
    targets: [
      { station_id: 'station-b', name: 'Beta', kind: 'native', ready: true },
      {
        station_id: 'station-c',
        name: 'Gamma',
        kind: 'branch_only',
        ready: false,
        failed: { code: 'move_not_ready', text: 'The branch is not pushed.' },
      },
    ],
  };
  const dialog = (initial: MoveRead = read) =>
    provide(<MoveDialog session={session()} onClose={noop} onStarted={noop} initial={initial} />);

  test('the title, the select, the six marks, the kind line and the two buttons', () => {
    const html = dialog();
    expect(html).toContain('aria-label="Move ada to another station"');
    expect(html).toContain('<option value="station-b" selected');
    expect(html).toContain('Gamma');
    expect(html).toContain('The conversation moves with its history.');
    for (const label of [
      'No turn is running',
      'No approval is open',
      'The branch is pushed',
      'The target has a home of this project',
      'No other move is open',
      'The session has confirmed its conversation',
    ]) {
      expect(html).toContain(label);
    }
    expect(html.match(/aria-label="Passed"/g)).toHaveLength(6);
    expect(html).toMatch(/<button type="submit" class="fd-primary">Move<\/button>/);
    expect(html).toContain('Cancel');
  });

  test('a target that is not ready has no ready station: Move is off, the marks show the refusal', () => {
    const html = dialog({ move: null, targets: [read.targets[1] as MoveRead['targets'][number]] });
    expect(html).toContain('<option value="" selected');
    expect(html).toMatch(/<button type="submit" class="fd-primary" disabled=""/);
    expect(html.match(/aria-label="Not checked"/g)).toHaveLength(6);
  });

  test('a station list without targets says so', () => {
    expect(dialog({ move: null, targets: [] })).toContain(
      'No other station has a home of this project.',
    );
  });

  test('before the read ends the dialog shows a loading line', () => {
    const html = provide(<MoveDialog session={session()} onClose={noop} onStarted={noop} />);
    expect(html).toContain('Reading the stations…');
  });
});

describe('the move banner', () => {
  const banner = (s: DeckSession, m: MoveView | null, owner = true, at: string = s.station_id) =>
    renderToStaticMarkup(
      <IntegrationsProvider actions={fakeActions()} initial={[synced()]}>
        <MoveBanner
          session={{ ...s, station_id: at }}
          stations={stations}
          move={m}
          owner={owner}
          onChanged={noop}
        />
      </IntegrationsProvider>,
    );

  test('an open move shows its progress as a status', () => {
    const html = banner(session(), move({ state: 'exporting', bytes: 2_097_152 }));
    expect(html).toContain('role="status"');
    expect(html).toContain('Moving: copying the conversation (2.0 MiB)');
  });

  test('a source that moved shows "Moved to" and the Open link', () => {
    const s = session({
      moved_to: { move_id: 'm1', station_id: 'station-b', session_id: 's2' },
    });
    const html = banner(s, move({ state: 'done' }));
    expect(html).toContain('Moved to Beta');
    expect(html).toContain('href="#/s/station-b/s2"');
    expect(html).toContain('>Open<');
    expect(html).not.toContain('Undo move');
  });

  test('a resume_failed move gives the owner "Undo move" and a member nothing', () => {
    const s = session({
      moved_to: { move_id: 'm1', station_id: 'station-b', session_id: 's2' },
    });
    const m = move({ state: 'resume_failed' });
    expect(banner(s, m)).toContain('Moved to Beta. The new session could not resume.');
    expect(banner(s, m)).toContain('>Undo move<');
    expect(banner(s, m, false)).not.toContain('Undo move');
  });

  test('the new session says it arrives, and shows the undo text when it could not resume', () => {
    const arriving = session({ move_in: { move_id: 'm1', state: 'launching' } });
    expect(banner(arriving, move({ state: 'starting' }), true, 'station-b')).toContain(
      'Arriving from Alpha…',
    );
    const failed = session({
      move_in: { move_id: 'm1', state: 'resume_failed', text: 'No conversation found' },
    });
    const html = banner(failed, move({ state: 'resume_failed' }), true, 'station-b');
    expect(html).toContain('Claude could not resume the conversation on Beta');
    expect(html).toContain('>Undo move<');
  });

  test('a failed move shows its text as an alert with a Dismiss button', () => {
    const html = banner(session(), move({ state: 'failed', message: 'The move stopped.' }));
    expect(html).toContain('role="alert"');
    expect(html).toContain('The move stopped.');
    expect(html).toContain('aria-label="Dismiss"');
  });

  test('a session without a move shows no banner', () => {
    expect(banner(session(), null)).toBe('');
    expect(banner(session(), move({ state: 'done' }))).toBe('');
  });
});

describe('Undo move', () => {
  test('the button shows; the confirm dialog opens only on the click', () => {
    const html = provide(<MoveUndo move="m1" source="Alpha" target="Beta" onUndone={noop} />);
    expect(html).toContain('>Undo move<');
    expect(html).not.toContain('Undo the move?');
  });
});

describe('the session menu and the pane', () => {
  const header = (s: DeckSession, scopes: string[], projects = [synced()]) => {
    const drive = sessionActions(
      async () => ({}),
      { station_id: 'station-a', session_id: 's1' },
      scopes,
    );
    const idle = { flags: sessionFlags(s, 'live', drive) } as never;
    return provide(
      <SessionHeader
        session={s}
        stations={stations}
        controller={
          { outcome: null, busy: null, act: noop, dismiss: noop, ...(idle as object) } as never
        }
        actions={drive}
        terminal={null}
      />,
      projects,
    );
  };

  test('"Move to station…" is for an admin login, in a project with sync on', () => {
    const all = ['read', 'drive', 'admin'];
    expect(header(session(), all)).toContain('Move to station…');
    expect(header(session(), ['read', 'drive'])).not.toContain('Move to station…');
    expect(header(session(), all, [project()])).not.toContain('Move to station…');
    const moved = session({
      moved_to: { move_id: 'm1', station_id: 'station-b', session_id: 's2' },
    });
    expect(header(moved, all)).not.toContain('Move to station…');
  });

  const view = (s: DeckSession) =>
    provide(
      <SessionView
        session={s}
        stations={stations}
        freshness={'live' as Freshness}
        timeline={undefined}
        actions={sessionActions(async () => ({}), { station_id: 'station-a', session_id: 's1' }, [
          'read',
          'drive',
          'approve',
        ])}
        layout="phone"
        tab="chat"
      />,
    );

  test('the composer is gone on a moved source and on a held target', () => {
    expect(view(session())).toContain('fd-composer');
    const moved = session({
      moved_to: { move_id: 'm1', station_id: 'station-b', session_id: 's2' },
    });
    const html = view(moved);
    expect(html).not.toContain('fd-composer');
    expect(html).toContain('Moved to Beta');
    const held = session({ move_in: { move_id: 'm1', state: 'resumed' } });
    expect(view(held)).not.toContain('fd-composer');
    expect(view(held)).toContain('Arriving from the source station…');
  });

  test('a released Claude target has the composer and the confirm line above it', () => {
    const html = view(session({ move_in: { move_id: 'm1', state: 'released' } }));
    expect(html).toContain('fd-composer');
    const line = html.indexOf('Send one prompt to confirm the moved conversation.');
    expect(line).toBeGreaterThan(-1);
    expect(line).toBeLessThan(html.indexOf('fd-composer'));
    expect(view(session({ move_in: { move_id: 'm1', state: 'confirmed' } }))).not.toContain(
      'Send one prompt to confirm',
    );
  });
});
