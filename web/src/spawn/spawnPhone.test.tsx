import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { StationInfo } from '../gen';
import { claudeCaps } from '../testing/capabilities';
import { station } from '../testing/fleet';
import { spawnActions } from './spawn';
import { SpawnForm } from './SpawnForm';

const info: StationInfo = {
  name: 'Alpha',
  incarnation: 'inc',
  head: 0,
  live_sessions: 0,
  version: '0.2.0',
  max_sessions: 4,
  allow_shell: null,
  providers: [
    {
      instance: 'claude',
      provider: 'claude',
      version: '2.1.999',
      auth: 'signed_in',
      version_status: 'unknown',
      capabilities: claudeCaps(),
    },
  ],
};

const reported = () => station('station-a', { info, info_at: 1 });

describe('the New session sheet on a phone', () => {
  test('a phone shows the board and scope reasons in the sticky footer; the folder note at its field', () => {
    // A narrow screen: the sheet has its phone layout (useNarrow), the form scrolls under the footer.
    const g = globalThis as { window?: unknown };
    const saved = g.window;
    g.window = {
      matchMedia: (query: string) => ({
        matches: query === '(max-width: 640px)',
        addEventListener: () => {},
        removeEventListener: () => {},
      }),
    };
    try {
      const html = renderToStaticMarkup(
        <SpawnForm
          stations={[reported()]}
          freshness="stale"
          onOpen={() => {}}
          actions={spawnActions(
            async () => ({ path: '/', parent: null, entries: [], truncated: false }),
            async () => ({}),
            ['read'],
          )}
        />,
      );
      const foot = html.slice(html.indexOf('class="fd-spawn-foot"'));
      expect(foot).toContain(
        '<div class="fd-spawn-status"><p class="fd-note" id="fd-spawn-scope">Starting sessions needs the drive scope.</p><p class="fd-note" id="fd-spawn-live">The form is off until the board is live.</p></div>',
      );
      // The folder note says what the Where row needs: it stays under the folder field.
      const where = html.slice(
        html.indexOf('aria-label="Directory"'),
        html.indexOf('fd-spawn-options'),
      );
      expect(where).toContain(
        '<p class="fd-note fd-spawn-need" id="fd-spawn-need">Choose a folder to start.</p>',
      );
      expect(html.match(/id="fd-spawn-need"/g)).toHaveLength(1);
      // The Batch switch is the first row of "More options", not a footer row: the footer is one
      // row of buttons.
      expect(foot).not.toContain('Batch');
      expect(html).toMatch(
        /<details class="fd-spawn-options"><summary class="fd-label"><span>More options<\/span><span class="fd-spawn-options-hint">Batch, coordinator, import<\/span><\/summary><div class="fd-spawn-options-body"><label class="fd-spawn-batch">/,
      );
      // Each note shows once: not also at the top of the sheet.
      expect(html.match(/id="fd-spawn-live"/g)).toHaveLength(1);
      expect(html.match(/id="fd-spawn-scope"/g)).toHaveLength(1);
      expect(html).toContain('aria-describedby="fd-spawn-scope fd-spawn-live fd-spawn-need"');
    } finally {
      g.window = saved;
    }
  });
});
