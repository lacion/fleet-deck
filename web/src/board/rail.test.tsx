// DL-141: the needs-you rail answers a request in place or opens its session.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { OpenRequest } from '../gen';
import { sessionActions } from '../session/commands';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { recordedRequest } from '../testing/timeline';
import { NeedsYouRail } from './NeedsYouRail';

const request = (extra: Partial<OpenRequest> = {}): OpenRequest => ({
  request_id: 'r1',
  node_id: null,
  request_kind: 'tool',
  title: 'Run tests',
  details: 'cargo test',
  input: null,
  options: [
    { option_id: 'allow', label: 'Allow' },
    { option_id: 'deny', label: 'Deny' },
  ],
  response: 'rpc_reply',
  requested_at: 1,
  ...extra,
});

const waiting = (open: OpenRequest, caps = claudeCaps()) =>
  card('s1', 'needs_you', 'station-a', {
    title: 'Fix login',
    capabilities: caps,
    open_requests: [open],
  });

const rail = (
  open: OpenRequest,
  scopes: string[] | null = ['read', 'drive', 'approve'],
  caps = claudeCaps(),
  freshness: 'live' | 'stale' = 'live',
) =>
  renderToStaticMarkup(
    <NeedsYouRail
      sessions={[waiting(open, caps)]}
      stations={[station()]}
      variant="side"
      freshness={freshness}
      onOpen={() => {}}
      sessionActions={
        scopes === null ? undefined : (key) => sessionActions(async () => ({}), key, scopes)
      }
    />,
  );

describe('needs-you rail', () => {
  test('an answerable request has one button per option: decide first, deny last', () => {
    const html = rail(request());
    expect(html).toContain('<h2 class="fd-rail-head"><span class="fd-rail-title">Needs you</span>');
    expect(html).toMatch(
      /<button type="button" class="fd-decide" aria-label="Allow: Run tests in Fix login">Allow<span class="fd-kbd">a<\/span><\/button>/,
    );
    expect(html).toMatch(
      /<button type="button" class="fd-danger" aria-label="Deny: Run tests in Fix login">Deny<span class="fd-kbd">d<\/span><\/button>/,
    );
    expect(html).not.toContain('Open session');
    expect(html.match(/aria-label="(Allow|Deny): /g)).toHaveLength(2);
  });

  test('the first request is selected and the options carry their key hints', () => {
    const html = rail(
      request({
        options: [
          { option_id: 'a', label: 'Once' },
          { option_id: 'b', label: 'Always' },
          { option_id: 'c', label: 'Deny' },
        ],
      }),
    );
    expect(html).toContain('data-request="r1" data-selected="true"');
    expect(html).toContain(
      '<span class="fd-rail-keys"><span class="fd-kbd">[</span><span class="fd-kbd">]</span> Move',
    );
    expect(html).toMatch(/Once<span class="fd-kbd">a<\/span>/);
    expect(html).toMatch(/Always<span class="fd-kbd">2<\/span>/);
    expect(html).toMatch(/Deny<span class="fd-kbd">d<\/span>/);
  });

  test('a single option is hinted with a, and only the first of two requests is selected', () => {
    const one = rail(request({ options: [{ option_id: 'ok', label: 'Okay' }] }));
    expect(one).toMatch(/Okay<span class="fd-kbd">a<\/span>/);
    const two = renderToStaticMarkup(
      <NeedsYouRail
        sessions={[
          waiting(request({ request_id: 'r1', requested_at: 1 })),
          card('s2', 'needs_you', 'station-a', {
            title: 'Other',
            capabilities: claudeCaps(),
            open_requests: [request({ request_id: 'r2', requested_at: 2 })],
          }),
        ]}
        stations={[station()]}
        variant="side"
        freshness="live"
        onOpen={() => {}}
        sessionActions={(key) =>
          sessionActions(async () => ({}), key, ['read', 'drive', 'approve'])
        }
      />,
    );
    expect(two.match(/data-selected="true"/g)).toHaveLength(1);
    expect(two).toContain('data-request="r1" data-selected="true"');
    expect(two).not.toContain('data-request="r2" data-selected');
  });

  test('without an answerable request there is no selection and no key hint', () => {
    const html = rail(request(), ['read', 'drive']);
    expect(html).not.toContain('data-selected');
    expect(html).not.toContain('fd-rail-keys');
    expect(html).not.toContain('fd-kbd');
  });

  test('a request without the approve scope, the capability or actions opens the session', () => {
    for (const html of [
      rail(request(), ['read', 'drive']),
      rail(request(), null),
      rail(request(), ['read', 'drive', 'approve'], { ...claudeCaps(), approvals: false }),
    ]) {
      expect(html).toContain('aria-label="Answer Run tests in Fix login"');
      expect(html).toContain('Open session');
      expect(html).not.toContain('aria-label="Allow: ');
    }
  });

  test('while the board is stale an otherwise answerable request opens the session', () => {
    const html = rail(request(), ['read', 'drive', 'approve'], claudeCaps(), 'stale');
    expect(html).toContain('aria-label="Answer Run tests in Fix login"');
    expect(html).toContain('Open session');
    expect(html).not.toContain('aria-label="Allow: ');
    expect(html).not.toContain('fd-rail-keys');
    expect(html).not.toContain('data-selected');
  });

  test('a question form is answered in the session, not in the rail', () => {
    const html = rail(recordedRequest('codex-question_rpc'));
    expect(html).toContain('Open session');
    expect(html).not.toContain('class="fd-decide"');
    expect(html).toMatch(/class="fd-q-kind"[^>]*>.*Question</);
  });

  test('an empty rail keeps only the notification switch on the side, and nothing as a strip', () => {
    const idle = [card('s1')];
    const side = renderToStaticMarkup(
      <NeedsYouRail
        sessions={idle}
        stations={[station()]}
        variant="side"
        freshness="live"
        onOpen={() => {}}
      />,
    );
    expect(side).toContain('class="fd-rail-mini"');
    expect(side).not.toContain('fd-rail-list');
    const strip = renderToStaticMarkup(
      <NeedsYouRail
        sessions={idle}
        stations={[station()]}
        variant="strip"
        freshness="live"
        onOpen={() => {}}
      />,
    );
    expect(strip).toBe('');
  });

  test('the where cell shows the branch over the station, and the bell goes to the header slot', () => {
    const html = renderToStaticMarkup(
      <NeedsYouRail
        sessions={[
          card('s1', 'needs_you', 'station-a', {
            title: 'Fix login',
            branch: 'fix/login',
            capabilities: claudeCaps(),
            open_requests: [request()],
          }),
        ]}
        stations={[station()]}
        variant="side"
        freshness="live"
        onOpen={() => {}}
      />,
    );
    expect(html).toMatch(
      /<span class="fd-q-where"><span class="fd-q-place" title="Branch: fix\/login"><svg[^>]*lucide-git-branch[\s\S]*?<\/svg><span class="fd-q-path">fix\/login<\/span><\/span><span class="fd-q-station">Alpha<\/span><\/span>/,
    );
    // nothing waits and the header slot is not mounted yet: no switch in place
    const idle = renderToStaticMarkup(
      <NeedsYouRail
        sessions={[card('s1')]}
        stations={[station()]}
        variant="side"
        freshness="live"
        onOpen={() => {}}
        bellSlot={null}
      />,
    );
    expect(idle).toBe('');
  });
});
