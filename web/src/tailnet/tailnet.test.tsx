import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { SettingsSaved } from '../gen';
import type { SettingsActions } from '../settings/actions';
import { DeckError, type DeckIo } from '../settings/http';
import {
  type Device,
  type PairRequest,
  type TailnetView,
  type WindowView,
  tailnetActions,
} from './api';
import {
  approveError,
  autoNotes,
  clockText,
  countdownText,
  groupDevices,
  groupFingerprint,
  osLabel,
  requestOf,
  scopeText,
  useButton,
  usesWindows,
  windowText,
} from './model';
import { saveDeckKey } from './save';
import { TailnetCardView, WINDOWS_TEXT, certificateLine } from './TailnetCard';
import { RequestCard } from './TailnetPairing';

const noop = () => {};

const VIEW: TailnetView = {
  available: true,
  self: { dns_name: 'deck.tail1234.ts.net', ips: ['100.64.0.1', 'fd7a::1'] },
  userspace: false,
  managed: { listen: ['100.64.0.1:7420'], link_listen: ['100.64.0.1:7421'] },
  current: { listen: ['127.0.0.1:7420'], link_listen: ['127.0.0.1:7421'] },
  // The key is absent from deck.toml: the mode is on (design 15.2).
  auto: { enabled: true, skipped: [], changed: false },
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
    renderToStaticMarkup(
      <TailnetCardView
        view={v}
        busy={false}
        note={note}
        onUse={noop}
        onAuto={noop}
        onCert={noop}
      />,
    );

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

describe('Tailscale for Windows (design 15.1)', () => {
  const windows = view({ program: 'windows' });
  const card = (v: TailnetView) =>
    renderToStaticMarkup(
      <TailnetCardView
        view={v}
        busy={false}
        note={null}
        onUse={noop}
        onAuto={noop}
        onCert={noop}
      />,
    );

  test('the text, and no "Use this program" button', () => {
    expect(usesWindows(windows)).toBe(true);
    const html = card(windows);
    expect(html).toContain(
      'Using Tailscale for Windows. It runs as your Windows user, with your Windows environment, the same as in a Windows terminal.',
    );
    expect(WINDOWS_TEXT).toContain('the same as in a Windows terminal.');
    expect(html).not.toContain('Use this program');
    expect(html).not.toContain('Tailscale for Windows is installed');
    // The normal card follows the text.
    expect(html).toContain('Board address');
    // The text shows also when Tailscale is not running.
    const down = card(view({ program: 'windows', available: false, reason: 'not running' }));
    expect(down).toContain('Using Tailscale for Windows.');
    expect(down).toContain('Tailscale is not available on the deck machine: not running.');
    expect(down).not.toContain('Use this program');
  });

  test('another program, or none, has no Windows text', () => {
    for (const program of ['configured', 'path', 'macos_app'] as const) {
      const v = view({ program });
      expect(usesWindows(v)).toBe(false);
      expect(card(v)).not.toContain('Using Tailscale for Windows');
    }
    expect(usesWindows(view())).toBe(false);
    const html = card(view({ available: false, reason: 'not installed' }));
    expect(html).toContain('Tailscale is not available on the deck machine: not installed.');
    expect(html).not.toContain('Use this program');
  });

  test('the WSL NAT reason shows in place of the userspace text', () => {
    const nat =
      'Tailscale runs on Windows and WSL uses NAT networking, so the tailnet address is not on this machine.';
    const html = card(view({ userspace: true, reason: nat }));
    expect(html).toContain(nat);
    expect(html).not.toContain('<button');
  });
});

const CERT_SAVED: SettingsSaved = {
  values: {},
  applied: [],
  restart_needed: [],
  unapplied: [],
  revision: 'rev-2',
  durability: 'confirmed',
};

describe('the toggle "Trusted certificate from Tailscale" (DL-190)', () => {
  const card = (v: TailnetView, busy = false) =>
    renderToStaticMarkup(
      <TailnetCardView view={v} busy={busy} note={null} onUse={noop} onAuto={noop} onCert={noop} />,
    );
  const cert = (c: NonNullable<TailnetView['certificate']>) => view({ certificate: c });

  test('the toggle, its help and an older deck without the field', () => {
    const off = card(cert({ enabled: false, state: 'off' }));
    expect(off).toContain('Trusted certificate from Tailscale');
    expect(off).toContain(
      'Browsers trust the board on its tailnet name. The name becomes public in Certificate Transparency logs.',
    );
    expect(off).not.toContain('Trusted certificate for');
    expect(card(view())).toContain('Trusted certificate from Tailscale');
    // The automatic toggle is on in VIEW, so two boxes are checked when this one is on.
    const on = card(cert({ enabled: true, state: 'renewing' }));
    expect(on.match(/checked=""/g)?.length).toBe(2);
    expect(card(cert({ enabled: true, state: 'renewing' }), true)).toContain('disabled=""');
  });

  test('a state line for each state', () => {
    expect(certificateLine(undefined)).toBeNull();
    expect(certificateLine({ enabled: false, state: 'off' })).toBeNull();
    expect(certificateLine({ enabled: true, state: 'renewing' })).toBe('Getting the certificate…');
    const end = Date.UTC(2027, 0, 15, 12);
    expect(
      certificateLine({ enabled: true, state: 'valid', name: 'deck.tail1.ts.net', not_after: end }),
    ).toBe(
      `Trusted certificate for deck.tail1.ts.net, valid until ${new Date(end).toLocaleDateString()}`,
    );
    expect(certificateLine({ enabled: true, state: 'expired' })).toBe(
      'The certificate expired. Fleet Deck is renewing it.',
    );
    expect(certificateLine({ enabled: true, state: 'name_mismatch' })).toBe(
      "This machine's tailnet name differs from the board's. Use tailnet again, then restart the deck.",
    );
  });

  test('the failure text by reason code', () => {
    const failed = (reason_code?: string) =>
      certificateLine({ enabled: true, state: 'failed', ...(reason_code ? { reason_code } : {}) });
    expect(failed('https_disabled')).toBe(
      'HTTPS certificates are off for your tailnet. Turn them on in the Tailscale admin console → DNS → HTTPS Certificates.',
    );
    expect(failed('not_logged_in')).toBe('Tailscale is not logged in on this machine.');
    expect(failed('timeout')).toBe('Tailscale did not answer in time.');
    expect(failed('not_installed')).toBe('Tailscale was not found.');
    expect(failed('store_failed')).toBe('The certificate could not be saved.');
    expect(failed('bad_certificate')).toBe('Tailscale could not issue a certificate.');
    expect(failed()).toBe('Tailscale could not issue a certificate.');
    expect(card(cert({ enabled: true, state: 'failed', reason_code: 'timeout' }))).toContain(
      'Tailscale did not answer in time.',
    );
  });

  test('turning it on asks first, then saves the deck key through the same flow', async () => {
    const calls: string[] = [];
    const sent: unknown[] = [];
    const settings: SettingsActions = {
      view: async () => ({ values: {}, fields: [], revision: 'rev-1', overrides: [] }),
      save: async (_t, change) => {
        calls.push('save');
        sent.push(change);
        return CERT_SAVED;
      },
      setSecret: async () => CERT_SAVED,
      elevate: async () => undefined,
      elevation: () => 'el-1',
      forgetElevation: () => undefined,
      restart: async () => ({ restarting: true, would_stop: [] }),
    };
    const ask = async () => {
      calls.push('ask');
      return 'el-1';
    };
    const labels = ['Tailscale: trusted certificate'];
    const out = await saveDeckKey(settings, ask, 'tailscale_cert', true, labels, true);
    expect(out).toEqual({ kind: 'saved' });
    expect(calls).toEqual(['ask', 'save']);
    expect(sent).toEqual([
      { changes: { tailscale_cert: true }, revision: 'rev-1', elevation: 'el-1' },
    ]);
  });
});

describe('the toggle "Use Tailscale automatically" (design 14.4)', () => {
  const card = (v: TailnetView, busy = false) =>
    renderToStaticMarkup(
      <TailnetCardView view={v} busy={busy} note={null} onUse={noop} onAuto={noop} onCert={noop} />,
    );

  test('off and on, with the help text', () => {
    const off = card(view({ auto: { enabled: false, skipped: [], changed: false } }));
    expect(off).toContain('Use Tailscale automatically');
    expect(off).toContain('type="checkbox"');
    expect(off).not.toContain('checked=""');
    expect(off).toContain(
      'Keeps the tailnet addresses current at each start. Applies after a restart.',
    );
    const on = card(view({ auto: { enabled: true, skipped: [], changed: false } }));
    expect(on).toContain('checked=""');
    // The deck sends the effective value: the key absent is on.
    expect(card(view())).toContain('checked=""');
    expect(card(view(), true)).toContain('disabled=""');
  });

  test('the toggle shows when Tailscale is not available or in userspace', () => {
    expect(card(view({ available: false, reason: 'not installed' }))).toContain('type="checkbox"');
    expect(card(view({ userspace: true }))).toContain('type="checkbox"');
  });

  test('the notes: the start reason, a skipped address and a change', () => {
    const auto = {
      enabled: true,
      start_reason: 'Tailscale did not answer within 3 seconds.',
      skipped: [{ address: '100.64.0.9:7420', reason: 'the address is not on this machine' }],
      changed: true,
    };
    expect(autoNotes(auto)).toEqual([
      'Not applied at start: Tailscale did not answer within 3 seconds.',
      '100.64.0.9:7420 not used: the address is not on this machine',
      'Your tailnet address changed. Restart the deck to use it.',
    ]);
    const html = card(view({ auto }));
    expect(html).toContain('Not applied at start: Tailscale did not answer within 3 seconds.');
    expect(html).toContain('100.64.0.9:7420 not used: the address is not on this machine');
    expect(html).toContain('Your tailnet address changed. Restart the deck to use it.');
    expect(autoNotes({ enabled: false, skipped: [], changed: false })).toEqual([]);
  });
});

describe('saveDeckKey', () => {
  const SAVED: SettingsSaved = {
    values: {},
    applied: [],
    restart_needed: [],
    unapplied: [],
    revision: 'rev-2',
    durability: 'confirmed',
  };

  /** `answers` are the replies of the saves, in order; a string is the code of a refusal. */
  function setup(answers: (SettingsSaved | string)[], held: string | null = null) {
    const calls: string[] = [];
    let elevation = held;
    const sent: unknown[] = [];
    const actions: SettingsActions = {
      view: async () => {
        calls.push('view');
        return { values: {}, fields: [], revision: 'rev-1', overrides: [] };
      },
      save: async (_t, change) => {
        calls.push('save');
        sent.push(change);
        const next = answers.shift();
        if (typeof next === 'string') {
          throw new DeckError(400, 'No.', next, next === 'elevation_required' ? { keys: [] } : {});
        }
        return next ?? SAVED;
      },
      setSecret: async () => SAVED,
      elevate: async () => {
        elevation = 'el-1';
      },
      elevation: () => elevation,
      forgetElevation: () => {
        elevation = null;
      },
      restart: async () => ({ restarting: true, would_stop: [] }),
    };
    const asked: (readonly string[])[] = [];
    const ask = async (labels: readonly string[] = []) => {
      calls.push('ask');
      asked.push(labels);
      elevation = 'el-1';
      return elevation;
    };
    const cancel = async () => {
      calls.push('ask');
      return null;
    };
    return { actions, calls, sent, asked, ask, cancel };
  }

  test('turning it on asks first, then saves the key', async () => {
    const t = setup([SAVED]);
    const out = await saveDeckKey(
      t.actions,
      t.ask,
      'tailnet_auto',
      true,
      ['Tailscale: use automatically'],
      true,
    );
    expect(out).toEqual({ kind: 'saved' });
    expect(t.calls).toEqual(['ask', 'view', 'save']);
    expect(t.asked).toEqual([['Tailscale: use automatically']]);
    expect(t.sent).toEqual([
      { changes: { tailnet_auto: true }, revision: 'rev-1', elevation: 'el-1' },
    ]);
  });

  test('a cancelled confirmation saves nothing', async () => {
    const t = setup([SAVED]);
    const out = await saveDeckKey(t.actions, t.cancel, 'tailnet_auto', true, ['x'], true);
    expect(out).toEqual({ kind: 'cancelled' });
    expect(t.calls).toEqual(['ask']);
  });

  test('turning it off asks only when the deck wants it, and retries once', async () => {
    const quiet = setup([SAVED]);
    expect(
      await saveDeckKey(quiet.actions, quiet.ask, 'tailnet_auto', false, ['x'], false),
    ).toEqual({ kind: 'saved' });
    expect(quiet.calls).toEqual(['view', 'save']);
    expect(quiet.sent).toEqual([
      { changes: { tailnet_auto: false }, revision: 'rev-1', elevation: null },
    ]);

    const asked = setup(['elevation_required', SAVED]);
    expect(
      await saveDeckKey(asked.actions, asked.ask, 'tailnet_auto', false, ['x'], false),
    ).toEqual({ kind: 'saved' });
    expect(asked.calls).toEqual(['view', 'save', 'ask', 'view', 'save']);

    // A second refusal is a failure, not a loop.
    const twice = setup(['elevation_required', 'elevation_required']);
    const out = await saveDeckKey(twice.actions, twice.ask, 'tailnet_auto', false, ['x'], false);
    expect(out.kind).toBe('failed');
    expect(twice.calls.filter((c) => c === 'ask')).toHaveLength(1);

    // A cancelled retry saves nothing more.
    const cancelled = setup(['elevation_required', SAVED]);
    expect(
      await saveDeckKey(cancelled.actions, cancelled.cancel, 'tailnet_auto', false, ['x'], false),
    ).toEqual({ kind: 'cancelled' });
    expect(cancelled.calls.filter((c) => c === 'save')).toHaveLength(1);
  });

  test('the failures have a text', async () => {
    const changed = setup(['settings_changed']);
    expect(await saveDeckKey(changed.actions, changed.ask, 'k', 1, [], false)).toEqual({
      kind: 'failed',
      text: 'These settings changed since you opened them.',
    });
    const bad = setup(['cannot_apply']);
    const out = await saveDeckKey(bad.actions, bad.ask, 'k', 1, [], false);
    expect(out.kind).toBe('failed');
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

  test('shows the heading, the grouped code, the scope words and both buttons', () => {
    const html = markup(REQUEST);
    expect(html).toContain('lab-box asks to join this deck');
    expect(html).toContain(
      'The command on lab-box printed a code. Check that it is the same as this code:',
    );
    expect(html).toContain(
      '<code class="fd-fingerprint">0123 4567 89ab cdef 0123 4567 89ab cdef 0123 4567 89ab cdef 0123 4567 89ab cdef</code>',
    );
    expect(html).toContain('If you add it, this deck can:');
    expect(html).toContain('<li>See its sessions and files</li>');
    expect(html).toContain('<li>Start sessions and send prompts</li>');
    expect(html).toContain('From 100.64.0.9');
    expect(html).toContain('Tailscale: lab · ann@example.com · tags tag:lab');
    expect(html).toContain('Ends at ');
    expect(html).toContain('>Yes, the codes match. Add lab-box</button>');
    expect(html).toContain('>No, refuse</button>');
    expect(html).not.toContain('role="alert"');
  });

  test('a hostile requested name renders as text', () => {
    const html = markup({ ...REQUEST, requested_name: '<img src=x onerror=alert(1)>' });
    expect(html).toContain('&lt;img src=x onerror=alert(1)&gt; asks to join this deck');
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

const device = (host: string, extra: Partial<Device> = {}): Device => ({
  host_name: host,
  dns_name: `${host}.tail1234.ts.net`,
  ips: ['100.64.0.9'],
  os: 'linux',
  tags: [],
  online: true,
  state: 'none',
  command: null,
  ...extra,
});

describe('groupDevices', () => {
  test('each group, in input order', () => {
    const groups = groupDevices([
      device('a'),
      device('b', { state: 'paired' }),
      device('c', { state: 'request waiting' }),
      device('d', { os: 'iOS' }),
      device('e', { online: false }),
      device('f'),
    ]);
    expect(groups.addable.map((d) => d.host_name)).toEqual(['a', 'f']);
    expect(groups.paired.map((d) => d.host_name)).toEqual(['b']);
    expect(groups.waiting.map((d) => d.host_name)).toEqual(['c']);
    expect(groups.unsupported.map((d) => d.host_name)).toEqual(['d']);
    expect(groups.offline.map((d) => d.host_name)).toEqual(['e']);
  });

  test('the priority is paired, waiting, unsupported, offline', () => {
    const phone = { os: 'android', online: false } as const;
    expect(groupDevices([device('x', { ...phone, state: 'paired' })]).paired).toHaveLength(1);
    expect(
      groupDevices([device('x', { ...phone, state: 'request waiting' })]).waiting,
    ).toHaveLength(1);
    expect(groupDevices([device('x', phone)]).unsupported).toHaveLength(1);
    expect(groupDevices([device('x', { online: false })]).offline).toHaveLength(1);
    for (const os of ['ios', 'ipados', 'android', 'tvos']) {
      expect(groupDevices([device('x', { os })]).unsupported).toHaveLength(1);
    }
    expect(groupDevices([])).toEqual({
      addable: [],
      waiting: [],
      paired: [],
      offline: [],
      unsupported: [],
    });
  });
});

describe('osLabel', () => {
  test('known systems, unknown and raw text', () => {
    expect(osLabel('linux')).toBe('Linux');
    expect(osLabel('macOS')).toBe('macOS');
    expect(osLabel('windows')).toBe('Windows');
    expect(osLabel('iOS')).toBe('iOS');
    expect(osLabel('ipados')).toBe('iPadOS');
    expect(osLabel('android')).toBe('Android');
    expect(osLabel(undefined)).toBe('Unknown system');
    expect(osLabel('freebsd')).toBe('freebsd');
  });
});

describe('requestOf', () => {
  const request = (source_ip: string, host?: string): PairRequest => ({
    request_id: `r-${source_ip}`,
    requested_name: 'x',
    station_fingerprint: HEX,
    offered_scopes: [],
    source_ip,
    admitted_at: 0,
    expires_at: 1,
    digest: 'd'.repeat(64),
    tailscale: host === undefined ? null : { host_name: host, tags: [] },
  });
  const lab = device('Lab-Box', { ips: ['100.64.0.9', 'fd7a::9'] });

  test('by source address', () => {
    const hit = request('fd7a::9');
    expect(requestOf(lab, [request('100.64.0.1'), hit])).toBe(hit);
  });
  test('by host name, ignoring case', () => {
    const hit = request('100.64.0.77', 'lab-BOX');
    expect(requestOf(lab, [request('100.64.0.1', 'other'), hit])).toBe(hit);
  });
  test('no match', () => {
    expect(requestOf(lab, [request('100.64.0.1', 'other'), request('100.64.0.2')])).toBeUndefined();
    expect(requestOf(lab, [])).toBeUndefined();
  });
});

describe('scopeText and countdownText', () => {
  test('each scope in plain words, an unknown scope as it is', () => {
    expect(scopeText('read')).toBe('See its sessions and files');
    expect(scopeText('drive')).toBe('Start sessions and send prompts');
    expect(scopeText('approve')).toBe('Answer approvals');
    expect(scopeText('admin')).toBe('Change its settings and manage it');
    expect(scopeText('teleport')).toBe('teleport');
  });
  test('m:ss', () => {
    expect(countdownText(600_000)).toBe('10:00');
    expect(countdownText(61_001)).toBe('1:02');
    expect(countdownText(9_000)).toBe('0:09');
    expect(countdownText(-5)).toBe('0:00');
  });
});
