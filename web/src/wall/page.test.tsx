// B17 §3.2.7: the page pane. A wall display shows the allowed page in a closed frame; the owner wall
// and the mirror show a card. The DOM side is kept small (`loadFrame`, `reloadOnce`) and tested with
// plain objects, the same as `control.ts`.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { wallDefaults } from '../app/routes';
import type { ArtifactActions } from '../artifacts/actions';
import type { DisplayPage, WallPage as WallPageEntry } from '../gen';
import { MIRROR_ARTIFACT_TEXT } from './ArtifactPane';
import { FRAME_TENTHS_MAX, longFrameTop } from './control';
import type { DisplayEnv } from './displayChanges';
import {
  BLOCKED_TEXT,
  BOARD_TEXT,
  DisplayPagePane,
  FRAME_ALLOW,
  FRAME_SANDBOX,
  hostOf,
  isFrameSrcViolation,
  loadFrame,
  OwnerPageView,
  PageCard,
  PageView,
  pageScrollTop,
  reloadOnce,
  reloadTarget,
  sameOrigin,
  screenOf,
  supportsCredentialless,
  UNSUPPORTED_TEXT,
} from './PagePane';
import { WallPage } from './WallPage';

const ORIGIN = 'https://deck.example';
const PAGE: DisplayPage = {
  page_id: 'P'.repeat(26),
  label: 'Grafana',
  url: 'https://grafana.example/d/abc',
  fit: 'screen',
};
const ENTRY: WallPageEntry = {
  page_id: 'P'.repeat(26),
  label: 'Grafana',
  url: 'https://grafana.example:3000/d/abc?token=x',
  origin: 'https://grafana.example:3000',
  fit: 'long',
  created_at: 1,
};

const view = (over: Partial<Parameters<typeof PageView>[0]> = {}) =>
  renderToStaticMarkup(
    <PageView
      state={{ kind: 'ready', page: PAGE }}
      supported
      origin={ORIGIN}
      blocked={false}
      {...over}
    />,
  );

describe('the page frame', () => {
  test('credentialless_is_set_before_src', () => {
    const log: string[] = [];
    const frame = {
      setAttribute: (name: string, value: string) => log.push(`attr:${name}=${value}`),
      set src(url: string) {
        log.push(`src:${url}`);
      },
      get src() {
        return '';
      },
    };
    loadFrame(frame, PAGE.url);
    expect(log).toEqual(['attr:credentialless=', `src:${PAGE.url}`]);
    // The markup has no `src` and no attribute that React does not know: `loadFrame` adds them.
    const html = view();
    expect(html).not.toContain('src=');
    expect(html).not.toContain('credentialless');
  });

  test('no_credentialless_support_shows_the_text', () => {
    expect(supportsCredentialless({ credentialless: false })).toBe(true);
    expect(supportsCredentialless({})).toBe(false);
    const html = view({ supported: false });
    expect(html).toContain(UNSUPPORTED_TEXT);
    expect(UNSUPPORTED_TEXT).toBe('This browser cannot show pages on a wall safely.');
    expect(html).not.toContain('<iframe');
  });

  test('a_same_origin_url_is_refused', () => {
    expect(sameOrigin('https://deck.example/board', ORIGIN)).toBe(true);
    expect(sameOrigin('https://deck.example:8443/', ORIGIN)).toBe(false);
    expect(sameOrigin('https://grafana.example/', ORIGIN)).toBe(false);
    expect(sameOrigin('not a url', ORIGIN)).toBe(false);
    const html = view({
      state: { kind: 'ready', page: { ...PAGE, url: 'https://deck.example/x' } },
    });
    expect(html).toContain(BOARD_TEXT);
    expect(BOARD_TEXT).toBe(
      'This is the address of the board. A wall cannot show the board in a frame.',
    );
    expect(html).not.toContain('<iframe');
  });

  test('the_frame_has_the_exact_sandbox_and_allow', () => {
    const html = view();
    expect(FRAME_SANDBOX).toBe('allow-scripts allow-same-origin allow-forms');
    expect(FRAME_ALLOW).toBe('');
    expect(html).toContain('sandbox="allow-scripts allow-same-origin allow-forms"');
    expect(html).toContain('allow=""');
    expect(html.toLowerCase()).toContain('referrerpolicy="no-referrer"');
    expect(html).toContain('class="fd-wall-pageframe"');
    expect(html).toContain('title="Grafana"');
    expect(html).toContain('data-wall-scroll="page"');
    expect(html).toContain('data-fit="screen"');
    // No top navigation and no popup flags.
    expect(html).not.toContain('allow-top-navigation');
    expect(html).not.toContain('allow-popups');
  });
});

describe('the reload for a new page', () => {
  const hash = '#/wall?mode=page&page=PPPP&pin=1';

  test('a_page_directive_without_page_gen_reloads_once', () => {
    expect(reloadTarget('', hash, 7)).toBe(`/display?page_gen=7${hash}`);
    expect(reloadTarget('?page_gen=6', hash, 7)).toBe(`/display?page_gen=7${hash}`);
    const calls: string[] = [];
    const target = reloadTarget('', hash, 7);
    const first = reloadOnce(null, target, (url) => calls.push(url));
    const second = reloadOnce(first, target, (url) => calls.push(url));
    expect(calls).toEqual([`/display?page_gen=7${hash}`]);
    expect(second).toBe(target);
    // The next page generation reloads as usual.
    reloadOnce(second, reloadTarget('', hash, 8), (url) => calls.push(url));
    expect(calls).toHaveLength(2);
    // No frame before the reload: the pane shows the loading line only.
    const env: DisplayEnv = { get: async () => PAGE, generation: 7 };
    const html = renderToStaticMarkup(
      <DisplayPagePane
        pageId={PAGE.page_id}
        env={env}
        tenths={0}
        where={{ search: '', hash }}
        replace={() => {}}
        supported
        origin={ORIGIN}
      />,
    );
    expect(html).not.toContain('<iframe');
    expect(html).toContain('Loading…');
  });

  test('with_page_gen_of_the_generation_there_is_no_reload', () => {
    expect(reloadTarget('?page_gen=7', hash, 7)).toBeNull();
    expect(reloadTarget('?x=1&page_gen=7', hash, 7)).toBeNull();
    const calls: string[] = [];
    expect(reloadOnce(null, null, (url) => calls.push(url))).toBeNull();
    expect(calls).toEqual([]);
  });

  test('a_frame_src_violation_shows_the_text_and_does_not_reload', () => {
    expect(isFrameSrcViolation({ effectiveDirective: 'frame-src' })).toBe(true);
    expect(isFrameSrcViolation({ effectiveDirective: 'script-src' })).toBe(false);
    expect(isFrameSrcViolation({})).toBe(false);
    const html = view({ blocked: true });
    expect(html).toContain(BLOCKED_TEXT);
    expect(BLOCKED_TEXT).toBe('The wall could not load this page. Reload the wall.');
    expect(html).not.toContain('<iframe');
    // The document has `page_gen` of its generation: the violation starts no second reload.
    expect(reloadTarget('?page_gen=7', '#/wall', 7)).toBeNull();
  });
});

describe('the long fit', () => {
  test('long_fit_follows_frame_tenths', () => {
    expect(pageScrollTop('long', 0, 1000)).toBe(0);
    expect(pageScrollTop('long', 30, 1000)).toBe(3000);
    expect(pageScrollTop('long', 30, 1000)).toBe(longFrameTop(30, 1000));
    expect(pageScrollTop('long', 999, 1000)).toBe((FRAME_TENTHS_MAX * 1000) / 10);
    // The one-screen fit never scrolls.
    expect(pageScrollTop('screen', 30, 1000)).toBe(0);
    const html = view({ state: { kind: 'ready', page: { ...PAGE, fit: 'long' } } });
    expect(html).toContain('data-fit="long"');
  });
});

describe('the owner wall and the mirror', () => {
  test('the_owner_wall_shows_a_card_and_no_frame', () => {
    const wall = renderToStaticMarkup(
      <WallPage
        route={{ ...wallDefaults, mode: 'page', page: { id: PAGE.page_id } }}
        navigate={() => {}}
        owner
        sessions={[]}
        stations={[]}
        freshness="live"
        initialSize={{ w: 1920, h: 1080 }}
        now={1_000_000}
        origin={ORIGIN}
      />,
    );
    expect(wall).toContain('data-mode="page"');
    expect(wall).not.toContain('<iframe');
    const card = renderToStaticMarkup(
      <OwnerPageView state={{ kind: 'ready', page: ENTRY }} tenths={0} />,
    );
    expect(card).toContain('Grafana');
    expect(card).toContain('class="fd-wall-pagehost">grafana.example<');
    expect(card).not.toContain('<iframe');
    expect(card).not.toContain('token=x');
  });

  test('the_mirror_shows_an_artifact_as_a_card_and_asks_for_no_view_link', () => {
    // A wall shows an artifact inert; an owner view link would run its scripts on this device.
    let views = 0;
    const artifacts = {
      view: () => {
        views += 1;
        return Promise.reject(new Error('no view link in a mirror'));
      },
    } as unknown as ArtifactActions;
    const html = (mirror: boolean) =>
      renderToStaticMarkup(
        <WallPage
          route={{
            ...wallDefaults,
            mode: 'artifact',
            artifact: { station: 'station-a', id: 'art', version: 2 },
          }}
          navigate={() => {}}
          owner
          sessions={[]}
          stations={[]}
          freshness="live"
          initialSize={{ w: 1920, h: 1080 }}
          now={1_000_000}
          origin={ORIGIN}
          artifacts={artifacts}
          mirror={mirror}
        />,
      );
    const card = html(true);
    expect(card).toContain(MIRROR_ARTIFACT_TEXT);
    expect(card).toContain('art · version 2');
    expect(card).not.toContain('<iframe');
    // The pane of the owner wall (which asks for the link) is not there.
    expect(card).not.toContain('Loading…');
    expect(views).toBe(0);
    // The owner's own wall keeps its pane.
    const own = html(false);
    expect(own).not.toContain(MIRROR_ARTIFACT_TEXT);
    expect(own).toContain('Loading…');
  });

  test('the_card_counts_screen_n_of_8_from_frame_tenths', () => {
    expect([0, 9, 10, 25, 69, 70, 200, -5].map(screenOf)).toEqual([1, 1, 2, 3, 7, 8, 8, 1]);
    const long = (tenths: number) =>
      renderToStaticMarkup(<PageCard page={ENTRY} tenths={tenths} />);
    expect(long(0)).toContain('Screen 1 of 8');
    expect(long(25)).toContain('Screen 3 of 8');
    expect(long(70)).toContain('Screen 8 of 8');
    // A page that fits one screen has no screen line.
    const one = renderToStaticMarkup(<PageCard page={{ ...ENTRY, fit: 'screen' }} tenths={25} />);
    expect(one).not.toContain('Screen ');
    expect(hostOf('http://[::1]:8080')).toBe('[::1]');
    expect(hostOf('http://localhost:3000')).toBe('localhost');
  });
});
