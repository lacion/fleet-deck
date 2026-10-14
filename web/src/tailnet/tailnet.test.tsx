import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeckIo } from '../settings/http';
import { type PairRequest, type TailnetView, type WindowView, tailnetActions } from './api';
import { approveError, clockText, groupFingerprint, useButton, windowText } from './model';
import { TailnetCardView } from './TailnetCard';
import { RequestCard } from './TailnetPairing';

const noop = () => {};

const VIEW: TailnetView = {
  available: true,
  self: { dns_name: 'deck.tail1234.ts.net', ips: ['100.64.0.1', 'fd7a::1'] },
  userspace: false,
  managed: { listen: ['100.64.0.1:7420'], link_listen: ['100.64.0.1:7421'] },
  current: { listen: ['127.0.0.1:7420'], link_listen: ['127.0.0.1:7421'] },
};

const view = (extra: Partial<TailnetView> = {}): TailnetView => ({ ...VIEW, ...extra });

const HEX = '0123456789abcdef'.repeat(4);

describe('tailnetActions (DL-178)', () => {
  const calls: { kind: 'get' | 'post'; path: string; body?: unknown }[] = [];
  const io: DeckIo = {
    get: async (path) => {
      calls.push({ kind: 'get', path });
      if (path === '/api/tailnet') return VIEW;
      if (path === '/api/tailnet/devices') return { available: true, devices: [] };
      if (path === '/api/pairing/window') return { open: false, generation: 0, deadline_ms: null };
      if (path === '/api/pairing/requests') {
        return { window: { open: false, generation: 0, deadline_ms: null }, requests: [] };
      }
      return {};
    },
    post: async (path, body) => {
      calls.push({ kind: 'post', path, body });
      if (path === '/api/tailnet/use')
        return { saved: true, restart_needed: true, listen: [], link_listen: [] };
      if (path === '/api/pairing/window') {
        return { open: true, generation: 1, deadline_ms: 5 };
      }
      if (path.endsWith('/approve')) return { approved: true, station_id: 'st-1' };
      return { denied: true };
    },
  };
  const actions = tailnetActions(io);

  test('the paths and bodies', async () => {
    calls.length = 0;
    expect((await actions.view()).available).toBe(true);
    expect((await actions.devices()).devices).toEqual([]);
    expect((await actions.window()).open).toBe(false);
    expect((await actions.requests()).requests).toEqual([]);
    expect((await actions.use('tok')).restart_needed).toBe(true);
    expect((await actions.setWindow(true, 'tok')).generation).toBe(1);
    await actions.setWindow(false, null);
    await actions.approve('req 1', 'f'.repeat(64), 'd'.repeat(64), 'tok');
    await actions.deny('req 1');
    expect(calls).toEqual([
      { kind: 'get', path: '/api/tailnet' },
      { kind: 'get', path: '/api/tailnet/devices' },
      { kind: 'get', path: '/api/pairing/window' },
      { kind: 'get', path: '/api/pairing/requests' },
      { kind: 'post', path: '/api/tailnet/use', body: { elevation: 'tok' } },
      { kind: 'post', path: '/api/pairing/window', body: { open: true, elevation: 'tok' } },
      { kind: 'post', path: '/api/pairing/window', body: { open: false } },
      {
        kind: 'post',
        path: '/api/pairing/requests/req%201/approve',
        body: { fingerprint: 'f'.repeat(64), digest: 'd'.repeat(64), elevation: 'tok' },
      },
      { kind: 'post', path: '/api/pairing/requests/req%201/deny', body: {} },
    ]);
  });

  test('the elevation is sent only when there is one', async () => {
    calls.length = 0;
    await actions.use(null);
    await actions.approve('r', 'f', 'd', null);
    expect(calls.map((c) => c.body)).toEqual([{}, { fingerprint: 'f', digest: 'd' }]);
  });

  test('a wrong shape is refused', async () => {
    const bad = tailnetActions({ get: async () => ({ nope: true }), post: async () => ({}) });
    await expect(bad.view()).rejects.toThrow('invalid tailnet view');
    await expect(bad.devices()).rejects.toThrow('invalid device list');
    await expect(bad.window()).rejects.toThrow('invalid pairing window');
    await expect(bad.requests()).rejects.toThrow('invalid request list');
  });
});

describe('useButton', () => {
  test('none, some and all of the managed entries are present', () => {
    expect(useButton(view())).toBe('use');
    expect(
      useButton(
        view({ current: { listen: ['100.64.0.1:7420'], link_listen: ['127.0.0.1:7421'] } }),
      ),
    ).toBe('update');
    expect(
      useButton(
        view({ current: { listen: ['100.64.0.1:7420'], link_listen: ['100.64.0.1:7421'] } }),
      ),
    ).toBe('none');
  });

  test('no button when Tailscale is not available or in userspace', () => {
    expect(useButton(view({ available: false }))).toBe('none');
    expect(useButton(view({ userspace: true }))).toBe('none');
  });
});

describe('groupFingerprint', () => {
  test('groups of 4 separated by one space', () => {
    expect(groupFingerprint(HEX)).toBe(
      Array.from({ length: 4 }, () => '0123 4567 89ab cdef'.split(' '))
        .flat()
        .join(' '),
    );
    expect(groupFingerprint('abcdef')).toBe('abcd ef');
    expect(groupFingerprint('')).toBe('');
  });
});

describe('windowText', () => {
  const NOW = Date.UTC(2026, 9, 14, 12, 0, 0);
  test('open', () => {
    const w: WindowView = { open: true, generation: 2, deadline_ms: NOW + 7 * 60_000 - 1_000 };
    expect(windowText(w, NOW)).toBe(
      `Accepting pairing requests until ${clockText(NOW + 7 * 60_000 - 1_000)} (7 min left).`,
    );
    expect(clockText(NOW)).toMatch(/^\d\d:\d\d$/);
  });
  test('closed', () => {
    expect(windowText({ open: false, generation: 2, deadline_ms: null }, NOW)).toBe(
      'The deck is not accepting pairing requests.',
    );
  });
});

describe('approveError', () => {
  test('each code', () => {
    expect(approveError('name_taken', 'x')).toBe(
      'Another station has this name. Deny the request and ask for another name.',
    );
    expect(approveError('already_paired', 'x')).toBe('This station key is already paired.');
    expect(approveError('window_closed', 'x')).toBe(
      'The window closed. Open it again and ask the station to retry.',
    );
    expect(approveError('digest_mismatch', 'x')).toBe(
      'The request changed. Check the fingerprint again.',
    );
    expect(approveError('fingerprint_mismatch', 'x')).toBe(
      'The request changed. Check the fingerprint again.',
    );
    expect(approveError('unknown_request', 'The deck text.')).toBe('The deck text.');
    expect(approveError(null, 'The deck text.')).toBe('The deck text.');
  });
});

describe('TailnetCardView', () => {
  const card = (v: TailnetView, note: string | null = null) =>
    renderToStaticMarkup(<TailnetCardView view={v} busy={false} note={note} onUse={noop} />);

  test('Tailscale is not available', () => {
    const html = card(view({ available: false, reason: 'tailscale is not installed' }));
    expect(html).toContain(
      'Tailscale is not available on the deck machine: tailscale is not installed. Pair stations with an invitation instead.',
    );
    expect(html).not.toContain('<button');
  });

  test('userspace networking', () => {
    const html = card(view({ userspace: true }));
    expect(html).toContain(
      'Tailscale runs in userspace networking: set the listen addresses by hand.',
    );
    expect(html).not.toContain('<button');
  });

  test('Use tailnet with the table of both lists', () => {
    const html = card(view());
    expect(html).toContain('This deck on the tailnet: deck.tail1234.ts.net (100.64.0.1, fd7a::1)');
    expect(html).toContain('Board address');
    expect(html).toContain('Link address');
    expect(html).toContain('Now');
    expect(html).toContain('With tailnet');
    expect(html).toContain('127.0.0.1:7420');
    expect(html).toContain('100.64.0.1:7421');
    expect(html).toContain('>Use tailnet</button>');
  });

  test('Update when some of the addresses are present', () => {
    const html = card(
      view({ current: { listen: ['100.64.0.1:7420'], link_listen: ['127.0.0.1:7421'] } }),
    );
    expect(html).toContain('>Update</button>');
    expect(html).not.toContain('Use tailnet</button>');
  });

  test('no button when all of the addresses are present', () => {
    const html = card(
      view({ current: { listen: ['100.64.0.1:7420'], link_listen: ['100.64.0.1:7421'] } }),
      'Saved.',
    );
    expect(html).toContain('The deck listens on its tailnet addresses.');
    expect(html).not.toContain('<button');
    expect(html).toContain('Saved.');
  });
});

describe('RequestCard', () => {
  const REQUEST: PairRequest = {
    request_id: 'req-1',
    requested_name: 'lab-box',
    station_fingerprint: HEX,
    offered_scopes: ['read', 'drive'],
    source_ip: '100.64.0.9',
    admitted_at: Date.UTC(2026, 9, 14, 12, 0),
    expires_at: Date.UTC(2026, 9, 14, 12, 10),
    digest: 'd'.repeat(64),
    tailscale: { host_name: 'lab', login: 'ann@example.com', tags: ['tag:lab'] },
  };
  const markup = (r: PairRequest, error: string | null = null) =>
    renderToStaticMarkup(<RequestCard request={r} onApprove={noop} onDeny={noop} error={error} />);

  test('shows the full fingerprint, the identity and both buttons', () => {
    const html = markup(REQUEST);
    expect(html).toContain('Requested name: lab-box');
    expect(html).toContain('Link key fingerprint:');
    expect(html).toContain(
      '<code class="fd-fingerprint">0123 4567 89ab cdef 0123 4567 89ab cdef 0123 4567 89ab cdef 0123 4567 89ab cdef</code>',
    );
    expect(html).toContain('Scopes: read, drive');
    expect(html).toContain('From 100.64.0.9');
    expect(html).toContain('Tailscale: lab · ann@example.com · tags tag:lab');
    expect(html).toContain('Expires ');
    expect(html).toContain(
      'Compare this fingerprint with the one that the station printed. Approve only when they are equal.',
    );
    expect(html).toContain('>Approve</button>');
    expect(html).toContain('>Deny</button>');
    expect(html).not.toContain('role="alert"');
  });

  test('a hostile requested name renders as text', () => {
    const html = markup({ ...REQUEST, requested_name: '<img src=x onerror=alert(1)>' });
    expect(html).toContain('Requested name: &lt;img src=x onerror=alert(1)&gt;');
    expect(html).not.toContain('<img');
  });

  test('the identity is unavailable', () => {
    const html = markup({ ...REQUEST, tailscale: { tags: [], error: 'whois failed' } });
    expect(html).toContain('Tailscale identity unavailable');
    expect(markup({ ...REQUEST, tailscale: null })).toContain('Tailscale identity unavailable');
  });

  test('an error shows as an alert', () => {
    expect(markup(REQUEST, 'This station key is already paired.')).toContain(
      '<p role="alert">This station key is already paired.</p>',
    );
  });
});
