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
) =>
  renderToStaticMarkup(
    <NeedsYouRail
      sessions={[waiting(open, caps)]}
      stations={[station()]}
      variant="side"
      onOpen={() => {}}
      sessionActions={
        scopes === null ? undefined : (key) => sessionActions(async () => ({}), key, scopes)
      }
    />,
  );

describe('needs-you rail', () => {
  test('an answerable request has one button per option, the first one primary', () => {
    const html = rail(request());
    expect(html).toContain('<h2 class="fd-rail-head"><span class="fd-rail-title">NEEDS YOU</span>');
    expect(html).toMatch(
      /<button type="button" class="fd-primary" aria-label="Allow: Run tests in Fix login">Allow<\/button>/,
    );
    expect(html).toMatch(
      /<button type="button" aria-label="Deny: Run tests in Fix login">Deny<\/button>/,
    );
    expect(html).not.toContain('Open session');
    expect(html.match(/aria-label="(Allow|Deny): /g)).toHaveLength(2);
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

  test('a question form is answered in the session, not in the rail', () => {
    const html = rail(recordedRequest('codex-question_rpc'));
    expect(html).toContain('Open session');
    expect(html).not.toContain('class="fd-primary"');
    expect(html).toContain('data-tone="info">Question<');
  });

  test('an empty rail says all clear; the strip variant renders nothing', () => {
    const idle = [card('s1')];
    const side = renderToStaticMarkup(
      <NeedsYouRail sessions={idle} stations={[station()]} variant="side" onOpen={() => {}} />,
    );
    expect(side).toContain('ALL CLEAR');
    expect(side).toContain('Nothing waits for you.');
    const strip = renderToStaticMarkup(
      <NeedsYouRail sessions={idle} stations={[station()]} variant="strip" onOpen={() => {}} />,
    );
    expect(strip).toBe('');
  });
});
