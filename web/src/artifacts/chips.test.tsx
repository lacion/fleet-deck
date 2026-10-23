import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { ChatView } from '../chat/ChatView';
import type { Event } from '../gen';
import { buildTimeline } from '../session/timeline';
import { envelopes } from '../testing/timeline';
import { ArtifactChip, SuggestedChip } from './ArtifactChips';
import { ArtifactsProvider, type ArtifactsEnv, keptKey } from './context';
import { fakeActions } from './fixtures';

const published = (id: string, title: string, version = 1): Event => ({
  kind: 'artifact.published',
  artifact_id: id,
  version,
  title,
  artifact_kind: 'html',
});
const suggested = (path: string): Event => ({ kind: 'artifact.suggested', path });

const items = (...events: Event[]) => buildTimeline(envelopes('s1', events)).items;

describe('chat chips from events', () => {
  test('artifact.published becomes an artifact item', () => {
    expect(items(published('a1', 'Report', 2))).toEqual([
      {
        type: 'artifact',
        key: '1',
        artifact_id: 'a1',
        version: 2,
        title: 'Report',
        artifact_kind: 'html',
      },
    ]);
  });

  test('artifact.suggested becomes a suggestion item at its place', () => {
    const list = items(suggested('out/report.html'), published('a1', 'Other'));
    expect(list.map((i) => i.type)).toEqual(['suggestion', 'artifact']);
    expect(list[0]).toMatchObject({ type: 'suggestion', path: 'out/report.html' });
  });
});

const env = (extra: Partial<ArtifactsEnv> = {}): ArtifactsEnv => ({
  actions: fakeActions(),
  session: { station: 'station-a', session: 's1' },
  ...extra,
});
const withEnv = (value: ArtifactsEnv | undefined, node: React.ReactNode) =>
  renderToStaticMarkup(<ArtifactsProvider value={value}>{node}</ArtifactsProvider>);

const chip = { artifact_id: 'a1', version: 2, title: 'Report', artifact_kind: 'html' as const };

describe('chips', () => {
  test('the published chip reads "Artifact · <title>" with its version apart, and opens the viewer', () => {
    const html = withEnv(env({ open: () => {} }), <ArtifactChip chip={chip} />);
    expect(html).toContain(
      '<span class="fd-art-chip-text">Artifact · Report</span><span class="fd-badge" data-tone="muted">v2</span>',
    );
    expect(html).toContain('<button type="button" class="fd-chip fd-art-chip"');
  });

  test('without an open handler the chip links to the artifact page', () => {
    const html = withEnv(env(), <ArtifactChip chip={chip} />);
    expect(html).toContain('href="#/artifact/station-a/a1/2"');
  });

  test('a chip of an event is hidden when a "Keep" chip published that version', () => {
    const kept = new Set([keptKey('a1', 2)]);
    expect(withEnv(env({ kept }), <ArtifactChip chip={chip} fromEvent />)).toBe('');
    expect(withEnv(env({ kept }), <ArtifactChip chip={chip} />)).toContain('Artifact · Report');
    expect(
      withEnv(env({ kept }), <ArtifactChip chip={{ ...chip, version: 3 }} fromEvent />),
    ).toContain('v3');
  });

  test('the suggested chip names the file and needs the drive scope', () => {
    const html = withEnv(env(), <SuggestedChip path="out/report.html" />);
    expect(html).toContain('Keep report.html as artifact');
    const readOnly = env({ actions: fakeActions([], { canPublish: false }) });
    expect(withEnv(readOnly, <SuggestedChip path="out/report.html" />)).toBe('');
    expect(withEnv(undefined, <SuggestedChip path="out/report.html" />)).toBe('');
  });

  test('the suggested chip shows its busy and error states', () => {
    const busy = withEnv(env(), <SuggestedChip path="r.html" initial={{ kind: 'busy' }} />);
    expect(busy).toContain('Publishing r.html…');
    expect(busy).toContain('disabled');
    const failed = withEnv(
      env(),
      <SuggestedChip path="r.html" initial={{ kind: 'error', text: 'outside the roots' }} />,
    );
    expect(failed).toContain('role="alert"');
    expect(failed).toContain('outside the roots');
    expect(failed).toContain('Keep r.html as artifact');
  });

  test('a kept suggestion turns into the published chip', () => {
    const html = withEnv(
      env({ open: () => {} }),
      <SuggestedChip path="r.html" initial={{ kind: 'done', chip }} />,
    );
    expect(html).toContain('Artifact · Report');
    expect(html).not.toContain('Keep r.html');
  });

  test('the chat view renders both chips', () => {
    const html = withEnv(
      env({ open: () => {} }),
      <ChatView
        items={items(suggested('r.html'), published('a1', 'Report'))}
        working={false}
        ready={true}
        error={null}
      />,
    );
    expect(html).toContain('Keep r.html as artifact');
    // Version 1 shows no version.
    expect(html).toContain('Artifact · Report</span></button>');
  });
});
