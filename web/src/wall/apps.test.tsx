// Wall apps (C2): the app branch of the display artifact pane, the feed, and the route flag.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { formatHash, parseHash, type WallRoute, wallDefaults } from '../app/routes';
import { appArtifactPath, appFeedPath, artifactPath, directiveRoute } from '../display/api';
import type { WallAppFeed } from '../gen';
import { routeDirective } from '../remote/mirror';
import { FEED_INTERVAL_MS, type FeedTimers, pollFeed, wallAppFeed } from './appFeed';
import { ArtifactBody, successState } from './DisplayArtifactPane';
import { WallAppFrame, wallView } from './WallAppFrame';

const html = (csp: string) =>
  new Response('<p>x</p>', {
    status: 200,
    headers: {
      'content-type': 'text/html; charset=utf-8',
      'content-security-policy': csp,
    },
  });
const APP_CSP = "sandbox allow-scripts; default-src 'none'; connect-src 'none'";
const INERT_CSP = "sandbox; default-src 'none'";

const URL_APP = appArtifactPath('st', 'art', 3);

describe('the app branch of the pane', () => {
  test('an_app_directive_renders_the_wall_app_frame', async () => {
    const state = await successState(html(APP_CSP), URL_APP, () => {}, true);
    expect(state).toEqual({ kind: 'app', url: URL_APP });
    const markup = renderToStaticMarkup(<ArtifactBody state={state} tenths={0} feed={undefined} />);
    expect(markup).toContain('sandbox="allow-scripts"');
    expect(markup).toContain('title="Wall app"');
    expect(markup).not.toContain('allow-same-origin');
    expect(markup).not.toContain(' allow=');
    expect(markup).toContain('data-fit="one"');
  });

  test('the_app_frame_src_declares_wall_apps_in_the_query', () => {
    expect(URL_APP).toBe(`${artifactPath('st', 'art', 3)}?features=wall_apps.v1`);
    const markup = renderToStaticMarkup(
      <WallAppFrame src={URL_APP} title="Wall app" data={undefined} />,
    );
    expect(markup).toContain('?features=wall_apps.v1');
    expect(wallView(URL_APP).url).toBe(URL_APP);
    expect(wallView(URL_APP).csp).toEqual({
      connect_domains: [],
      resource_domains: [],
    });
  });

  test('the_probe_uses_the_same_url_as_the_frame', async () => {
    // The pane gives one `url` to the probe and to `successState`, which puts it into the frame.
    const state = await successState(html(APP_CSP), URL_APP, () => {}, true);
    expect(state.kind === 'app' && state.url).toBe(URL_APP);
    const plain = await successState(
      html(INERT_CSP),
      artifactPath('st', 'art', 3),
      () => {},
      false,
    );
    expect(plain.kind === 'frame' && plain.url).toBe(artifactPath('st', 'art', 3));
  });

  test('an_inert_answer_to_an_app_probe_gives_the_inert_frame', async () => {
    const state = await successState(html(INERT_CSP), URL_APP, () => {}, true);
    expect(state).toEqual({ kind: 'frame', url: URL_APP });
    const markup = renderToStaticMarkup(<ArtifactBody state={state} tenths={0} />);
    expect(markup).toContain('sandbox=""');
  });

  test('without_app_the_frame_stays_inert', async () => {
    const state = await successState(html(APP_CSP), artifactPath('st', 'art', 3), () => {});
    expect(state.kind).toBe('frame');
    const markup = renderToStaticMarkup(<ArtifactBody state={state} tenths={0} />);
    expect(markup).toContain('sandbox=""');
    expect(markup).not.toContain('allow-scripts');
  });
});

describe('the feed', () => {
  const feed = (at: number): WallAppFeed => ({ at, sessions: [] });
  const fakeTimers = () => {
    const queue: { fn: () => void; ms: number; live: boolean }[] = [];
    const timers: FeedTimers = {
      set: (fn, ms) => {
        const t = { fn, ms, live: true };
        queue.push(t);
        return t;
      },
      clear: (h) => {
        (h as { live: boolean }).live = false;
      },
    };
    const tick = async () => {
      const t = queue.find((q) => q.live);
      if (t === undefined) throw new Error('no timer');
      t.live = false;
      t.fn();
      for (let i = 0; i < 8; i += 1) await Promise.resolve();
    };
    return { timers, queue, tick };
  };
  const flush = async () => {
    for (let i = 0; i < 8; i += 1) await Promise.resolve();
  };

  test('the_feed_is_polled_and_passed_as_data', async () => {
    const t = fakeTimers();
    const paths: string[] = [];
    let n = 0;
    const got: WallAppFeed[] = [];
    const stop = pollFeed(
      async (path) => {
        paths.push(path);
        n += 1;
        return feed(n);
      },
      appFeedPath('st', 'a b', 3),
      (f) => got.push(f),
      t.timers,
    );
    await flush();
    expect(paths).toEqual(['/api/displays/self/app-feed/st/a%20b/3']);
    expect(got).toEqual([feed(1)]);
    expect(t.queue[0]?.ms).toBe(FEED_INTERVAL_MS);
    await t.tick();
    expect(got).toEqual([feed(1), feed(2)]);
    stop();
    expect(t.queue.every((q) => !q.live)).toBe(true);
    expect(paths).toHaveLength(2);
  });

  test('a_failed_poll_keeps_the_last_feed', async () => {
    const t = fakeTimers();
    const answers: (() => Promise<unknown>)[] = [
      async () => feed(1),
      async () => {
        throw new Error('404');
      },
      async () => ({ at: 'x' }),
      async () => feed(4),
    ];
    const got: WallAppFeed[] = [];
    pollFeed(
      () => (answers.shift() ?? (async () => feed(9)))(),
      '/p',
      (f) => got.push(f),
      t.timers,
    );
    await flush();
    await t.tick();
    await t.tick();
    expect(got).toEqual([feed(1)]);
    await t.tick();
    expect(got).toEqual([feed(1), feed(4)]);
  });

  test('the_feed_is_validated', () => {
    expect(() => wallAppFeed({ at: 1 })).toThrow();
    expect(() => wallAppFeed({ at: 1, sessions: {} })).toThrow();
    expect(wallAppFeed({ at: 1, sessions: [] })).toEqual({
      at: 1,
      sessions: [],
    });
  });
});

describe('the app flag of the route', () => {
  const route: WallRoute = {
    ...wallDefaults,
    mode: 'artifact',
    artifact: { station: 'st', id: 'art', version: 3, app: true },
  };

  test('the_app_flag_round_trips_through_the_wall_hash', () => {
    const hash = formatHash(route);
    expect(hash).toContain('app=1');
    const back = parseHash(hash);
    expect(back.name === 'wall' && back.artifact).toEqual({
      station: 'st',
      id: 'art',
      version: 3,
      app: true,
    });
    const plain = formatHash({
      ...route,
      artifact: { station: 'st', id: 'art', version: 3 },
    });
    expect(plain).not.toContain('app=');
    const parsed = parseHash(plain);
    expect(parsed.name === 'wall' && parsed.artifact?.app).toBeUndefined();
    expect(routeDirective(route, false, null)).toMatchObject({
      mode: 'artifact',
      app: true,
    });
  });

  test('a_directive_with_app_gives_a_route_with_app', () => {
    const body = {
      mode: 'artifact' as const,
      artifact: { station_id: 'st', artifact_id: 'art', version: 3 },
      app: true,
      pin: true,
      filter: null,
    };
    expect(directiveRoute(body).artifact).toEqual({
      station: 'st',
      id: 'art',
      version: 3,
      app: true,
    });
    expect(directiveRoute({ ...body, app: false }).artifact).toEqual({
      station: 'st',
      id: 'art',
      version: 3,
    });
  });
});
