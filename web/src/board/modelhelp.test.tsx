// DL-167 (R10): model help on the board. Generated titles carry a mark, a
// digest shows as one line on the card and in the session details, the
// session menu has Summarize, and the usage meters show the station's use
// under "Fleet Deck". A missing text shows nothing.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckSession, StationInfo } from '../gen';
import { sessionActions } from '../session/commands';
import { sessionFlags } from '../session/controller';
import { SessionHeader } from '../session/SessionHeader';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { FleetBoard } from './FleetBoard';
import { UsageMeters } from './UsageMeters';
import { assistMeters, assistText } from './usage';

const key = { station_id: 'station-a', session_id: 's1' };
const drive = sessionActions(async () => ({}), key, ['read', 'drive']);
const readOnly = sessionActions(async () => ({}), key, ['read']);

const info = (extra: Partial<StationInfo> = {}): StationInfo => ({
  name: 'alpha',
  incarnation: 'inc',
  head: 0,
  live_sessions: 0,
  version: null,
  max_sessions: null,
  allow_shell: null,
  providers: [],
  ...extra,
});

function generated(extra: Partial<DeckSession['summary']> = {}): DeckSession {
  return card('s1', 'idle', 'station-a', {
    title: 'Fix the login redirect',
    title_generated: true,
    capabilities: claudeCaps(),
    ...extra,
  });
}

const board = (s: DeckSession) =>
  renderToStaticMarkup(<FleetBoard sessions={[s]} stations={[station()]} freshness="live" />);

const idle = {
  busy: null,
  outcome: null,
  act: () => {},
  dismiss: () => {},
};

function header(s: DeckSession, actions: typeof drive | null = drive) {
  return renderToStaticMarkup(
    <SessionHeader
      session={s}
      stations={[station()]}
      controller={{ ...idle, flags: sessionFlags(s, 'live', actions) }}
      actions={actions}
      terminal={null}
    />,
  );
}

describe('generated titles and digests', () => {
  test('a generated title has the mark on the card, and an owner title has none', () => {
    expect(board(generated())).toContain('<span class="fd-generated"');
    expect(board(generated({ title_generated: false }))).not.toContain('fd-generated');
    // The flag alone is not enough: no title, no mark.
    expect(board(generated({ title: null }))).not.toContain('fd-generated');
  });

  test('the card shows the first digest line, without the list marker', () => {
    const html = board(generated({ digest: '- Read the router\n- Fixed the redirect' }));
    expect(html).toContain('class="fd-card-digest"');
    expect(html).toContain('>Read the router<');
    expect(html).not.toContain('Fixed the redirect');
    expect(board(generated())).not.toContain('fd-card-digest');
  });

  test('the session header marks a generated title and lists the digest in the details', () => {
    const html = header(generated({ digest: '- Read the router\n- Fixed the redirect' }));
    expect(html).toContain('<span class="fd-generated"');
    expect(html).toContain('Summary (generated)');
    expect(html).toContain('<li>Read the router</li><li>Fixed the redirect</li>');
    const plain = header(generated({ title_generated: false }));
    expect(plain).not.toContain('fd-generated');
    expect(plain).not.toContain('Summary (generated)');
  });
});

describe('Summarize in the session menu', () => {
  test('a live session that the owner may drive offers Summarize, also when it ended', () => {
    expect(header(generated())).toMatch(/<button[^>]*>(<svg[\s\S]*?<\/svg>)? Summarize<\/button>/);
    const ended = generated({ state: 'exited', current_run: null });
    expect(header(ended)).toContain(' Summarize</button>');
  });

  test('a read-only login has no Summarize item', () => {
    expect(header(generated(), readOnly)).not.toContain('Summarize');
  });

  test('Summarize posts to the digest route of the session and says what happened', async () => {
    const posts: [string, unknown][] = [];
    const ok = sessionActions(
      async (path, body) => {
        posts.push([path, body]);
        return { text: 'generated', generated: true, cached: false };
      },
      key,
      ['drive'],
    );
    expect(await ok.summarize()).toEqual({ ok: true, text: 'Summarized.' });
    expect(posts).toEqual([['/api/stations/station-a/sessions/s1/digest', {}]]);
    const off = sessionActions(
      async () => {
        throw new Error('model help is switched off in station.toml');
      },
      key,
      ['drive'],
    );
    expect(await off.summarize()).toEqual({
      ok: false,
      text: 'No summary was made: model help is switched off in station.toml',
    });
  });
});

describe('model help in the usage meters', () => {
  test('a station with a cap has one Fleet Deck meter; a station without one has none', () => {
    const on = station('station-a', {
      info: info({ assist_tokens_today: 1200, assist_cap: 200000 }),
    });
    const off = station('station-b', { info: info() });
    const noInfo = station('station-c');
    const meters = assistMeters([off, on, noInfo]);
    expect(meters).toEqual([
      { key: 'station-a/fleetdeck', label: 'Alpha · Fleet Deck', tokens: 1200, cap: 200000 },
    ]);
    expect(assistText(meters[0] ?? { key: '', label: '', tokens: 0, cap: 0 })).toBe(
      '1,200 of 200,000 tokens today',
    );
  });

  test('the usage panel shows the meter under the Fleet Deck scope', () => {
    const on = station('station-a', {
      info: info({ assist_tokens_today: 1200, assist_cap: 200000 }),
    });
    const html = renderToStaticMarkup(<UsageMeters sessions={[]} stations={[on]} />);
    expect(html).toContain('aria-label="Fleet Deck model help"');
    expect(html).toContain('Alpha · Fleet Deck');
    expect(html).toContain('1,200 of 200,000 tokens today');
    expect(html.match(/<meter /g)?.length).toBe(1);
    const none = renderToStaticMarkup(<UsageMeters sessions={[]} stations={[station()]} />);
    expect(none).not.toContain('Fleet Deck');
  });
});
