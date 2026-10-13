import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FleetBoard } from '../board/FleetBoard';
import { FileView } from '../session/FilesPanel';
import { sessionActions } from '../session/commands';
import { type PaneTab, SessionView } from '../session/SessionView';
import { card, station } from '../testing/fleet';
import { summary } from '../testing/summary';
import { claudeCapabilities } from '../testing/timeline';
import { ArtifactsProvider } from './context';
import { fakeActions } from './fixtures';

const session = (count: number) => ({
  station_id: 'station-a',
  status: 'idle' as const,
  summary: summary('s1', 'idle', 1, {
    title: 'Review',
    capabilities: claudeCapabilities(),
    artifacts_count: count,
  }),
});

const view = (count: number, tab: PaneTab = 'chat', provided = true) => {
  const node = (
    <SessionView
      session={session(count)}
      stations={[station()]}
      freshness="live"
      timeline={undefined}
      actions={sessionActions(async () => ({}), { station_id: 'station-a', session_id: 's1' }, [
        'read',
        'drive',
      ])}
      layout="tablet"
      tab={tab}
    />
  );
  return renderToStaticMarkup(
    provided ? (
      <ArtifactsProvider value={{ actions: fakeActions() }}>{node}</ArtifactsProvider>
    ) : (
      node
    ),
  );
};

describe('the session Artifacts tab', () => {
  test('the tab sits beside Chat and Changes, with the count', () => {
    const html = view(2);
    expect(html).toMatch(/role="tab"[^>]*>Artifacts <span class="fd-count">2<\/span>/);
    expect(html.indexOf('>Changes')).toBeLessThan(html.indexOf('>Artifacts'));
  });

  test('no count when there is none; no tab without the artifact calls', () => {
    expect(view(0)).toMatch(/role="tab"[^>]*>Artifacts<\/button>/);
    expect(view(2, 'chat', false)).not.toContain('Artifacts');
  });

  test('the artifacts tab shows the list; without the calls it falls back to the chat', () => {
    expect(view(1, 'artifacts')).toContain('Loading the artifacts…');
    const off = view(1, 'artifacts', false);
    expect(off).toContain('aria-label="Conversation"');
  });
});

describe('Publish as artifact', () => {
  test('the file view offers it for a publishable file only', () => {
    const file = (path: string) => ({
      path,
      size: 3,
      binary: false,
      truncated: false,
      text: 'abc',
    });
    const render = (path: string, onPublish?: () => void) =>
      renderToStaticMarkup(
        <FileView file={file(path)} root={null} onBack={() => {}} onPublish={onPublish} />,
      );
    expect(render('out/report.html', () => {})).toContain('Publish as artifact');
    expect(render('notes.md', () => {})).toContain('Publish as artifact');
    expect(render('src/main.rs', () => {})).not.toContain('Publish as artifact');
    expect(render('out/report.html')).not.toContain('Publish as artifact');
  });
});

describe('cards', () => {
  const board = (extra: Parameters<typeof card>[3]) =>
    renderToStaticMarkup(
      <FleetBoard
        sessions={[card('s1', 'working', 'station-a', extra)]}
        stations={[station()]}
        freshness="live"
      />,
    );

  test('a card shows the focus line once and the artifacts count chip', () => {
    const html = board({ focus: 'Porting the parser', artifacts_count: 3 });
    expect(html.match(/Porting the parser/g)?.length).toBe(2); // the text and its title attribute
    expect(html.match(/class="fd-focus"/g)?.length).toBe(1);
    expect(html).toContain('data-artifacts="3"');
  });

  test('no focus and no artifacts: neither shows', () => {
    const html = board({ artifacts_count: 0 });
    expect(html).not.toContain('fd-focus');
    expect(html).not.toContain('data-artifacts');
    expect(board({ focus: '' })).not.toContain('fd-focus');
  });
});
