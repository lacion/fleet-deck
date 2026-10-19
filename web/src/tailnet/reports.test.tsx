import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { DeckError, type DeckHttp } from '../settings/http';
import { clockText, groupFingerprint } from './model';
import {
  CODE_TEXTS,
  codeInfo,
  INVALID_ANSWER,
  isReport,
  isRetry,
  isUnavailable,
  isWindow,
  newRequestId,
  pruneRequestIds,
  type ReportsActions,
  type ReportsView,
  reportsActions,
  requestIdFor,
  retryError,
  rowControl,
  type SetupReport,
} from './reports';
import {
  type Ask,
  allowOneRetry,
  changeWindow,
  plainCall,
  type ReportsState,
  type RowHandlers,
  SetupReportsView,
} from './SetupReports';

const FP = '0123456789abcdef'.repeat(4);
const TERMINAL = 'a'.repeat(32);
const DIGEST = 'd'.repeat(64);
const noop = () => {};

const report = (extra: Partial<SetupReport> = {}): SetupReport => ({
  fingerprint: FP,
  name: 'lab-box',
  code: 'pairing_failed',
  phase: 'pairing',
  received_at: 1_700_000_000_000,
  last_contact: 1_700_000_300_000,
  terminal_id: TERMINAL,
  state_digest: DIGEST,
  retry: null,
  ...extra,
});

const handlers: RowHandlers = {
  onAsk: noop,
  onConfirm: noop,
  onDismissAsk: noop,
  onCancelRetry: noop,
  onDismiss: noop,
};

const ready = (
  reports: SetupReport[],
  win: { open: boolean; deadline_ms: number | null } = { open: false, deadline_ms: null },
): ReportsState => ({ kind: 'ready', view: { window: win, reports } });

function render(
  state: ReportsState,
  extra: { confirming?: string | null; errors?: Record<string, string> } = {},
) {
  return renderToStaticMarkup(
    <SetupReportsView
      state={state}
      busy={null}
      windowBusy={false}
      windowError={null}
      confirming={extra.confirming ?? null}
      errors={extra.errors ?? {}}
      onOpen={noop}
      onClose={noop}
      handlersOf={() => handlers}
    />,
  );
}

describe('the validators', () => {
  test('a window', () => {
    expect(isWindow({ open: false, deadline_ms: null })).toBe(true);
    expect(isWindow({ open: true, deadline_ms: 5 })).toBe(true);
    expect(isWindow({ open: 'yes', deadline_ms: null })).toBe(false);
    expect(isWindow({ open: true })).toBe(false);
    expect(isWindow(null)).toBe(false);
  });

  test('a retry', () => {
    expect(isRetry({ state: 'queued', expires_at: 1 })).toBe(true);
    expect(isRetry({ state: 'delivered', expires_at: 1 })).toBe(true);
    expect(isRetry({ state: 'started', expires_at: 1 })).toBe(false);
    expect(isRetry({ state: 'queued' })).toBe(false);
  });

  test('a report', () => {
    expect(isReport(report())).toBe(true);
    expect(isReport(report({ retry: { state: 'queued', expires_at: 9 } }))).toBe(true);
    expect(isReport({ ...report(), retry: undefined })).toBe(false);
    expect(isReport({ ...report(), received_at: 'now' })).toBe(false);
    const { name: _name, ...noName } = report();
    expect(isReport(noName)).toBe(false);
  });
});

describe('reportsActions', () => {
  const calls: { kind: string; path: string; body?: unknown }[] = [];
  const view: ReportsView = { window: { open: false, deadline_ms: null }, reports: [report()] };
  const io: DeckHttp = {
    get: async (path) => {
      calls.push({ kind: 'get', path });
      return view;
    },
    post: async (path, body) => {
      calls.push({ kind: 'post', path, body });
      if (path.endsWith('/window')) return { open: true, deadline_ms: 9 };
      return { retry: { state: 'queued', expires_at: 7 } };
    },
    del: async (path) => {
      calls.push({ kind: 'del', path });
      return null;
    },
  };
  const actions = reportsActions(io);

  test('the paths and the bodies', async () => {
    calls.length = 0;
    expect((await actions.list()).reports).toHaveLength(1);
    expect(await actions.setWindow(true, 'tok')).toEqual({ open: true, deadline_ms: 9 });
    await actions.setWindow(false, null);
    expect(await actions.allowRetry(FP, TERMINAL, DIGEST, 'r'.repeat(32), 'tok')).toEqual({
      state: 'queued',
      expires_at: 7,
    });
    await actions.cancelRetry(FP);
    await actions.dismiss(FP);
    expect(calls).toEqual([
      { kind: 'get', path: '/api/provision-reports' },
      {
        kind: 'post',
        path: '/api/provision-reports/window',
        body: { open: true, elevation: 'tok' },
      },
      { kind: 'post', path: '/api/provision-reports/window', body: { open: false } },
      {
        kind: 'post',
        path: `/api/provision-reports/${FP}/retry`,
        body: {
          terminal_id: TERMINAL,
          state_digest: DIGEST,
          request_id: 'r'.repeat(32),
          elevation: 'tok',
        },
      },
      { kind: 'del', path: `/api/provision-reports/${FP}/retry` },
      { kind: 'del', path: `/api/provision-reports/${FP}` },
    ]);
  });

  test('an invalid answer is refused with one text', async () => {
    const bad = reportsActions({
      get: async () => ({ window: { open: false, deadline_ms: null }, reports: [{ nope: 1 }] }),
      post: async () => ({ nope: true }),
      del: async () => null,
    });
    await expect(bad.list()).rejects.toThrow(INVALID_ANSWER);
    await expect(bad.setWindow(true, 'tok')).rejects.toThrow(INVALID_ANSWER);
    await expect(bad.allowRetry(FP, TERMINAL, DIGEST, 'r', 'tok')).rejects.toThrow(INVALID_ANSWER);
    expect(INVALID_ANSWER).toBe('The deck sent an invalid answer.');
    const noWindow = reportsActions({
      get: async () => ({ reports: [] }),
      post: async () => ({}),
      del: async () => null,
    });
    await expect(noWindow.list()).rejects.toThrow(INVALID_ANSWER);
  });

  test('a 503 pairing_unavailable is the hidden case', () => {
    expect(isUnavailable(new DeckError(503, 'x', 'pairing_unavailable', {}))).toBe(true);
    expect(isUnavailable(new DeckError(503, 'x', 'other', {}))).toBe(false);
    expect(isUnavailable(new DeckError(404, 'x', 'pairing_unavailable', {}))).toBe(false);
    expect(isUnavailable(new Error('x'))).toBe(false);
  });
});

describe('the code texts (design 4.5)', () => {
  const TABLE: [string, string, boolean][] = [
    ['file_invalid', 'The provisioning file is missing, unsafe, or invalid.', false],
    [
      'release_untrusted',
      'The trusted manifest or an installed hook file is missing, is not owned by root, or does not match its hash.',
      false,
    ],
    [
      'build_mismatch',
      'The running build or the binary of the station does not match the trusted manifest.',
      false,
    ],
    ['config_conflict', 'An existing station configuration conflicts with this setup.', false],
    ['config_changed', 'The provisioning file changed after the pairing started.', false],
    [
      'service_conflict',
      'The station service that is installed has another binary or another data folder.',
      false,
    ],
    [
      'station_identity_changed',
      'The station key is not the key that the bootstrap saved first.',
      false,
    ],
    [
      'paired_elsewhere',
      'The station is already paired with a deck that has another fingerprint.',
      false,
    ],
    ['denied', 'The pairing request was refused.', true],
    ['cancelled', 'The pairing request was cancelled.', true],
    ['revoked', 'The pairing request or authorization was revoked.', true],
    ['fingerprint_mismatch', 'The peer did not match the configured deck fingerprint.', false],
    [
      'request_id_conflict',
      'The pairing request identifier conflicts with an earlier request.',
      false,
    ],
    ['pending', 'An earlier pairing request is still pending.', true],
    ['unsupported', 'A required setup operation is not supported by this build.', false],
    ['invalid_request', 'A setup request was rejected as invalid.', false],
    ['retry_file', 'Fleet Deck could not safely use the saved pairing recovery file.', false],
    ['pairing_failed', 'Pairing did not finish.', true],
    [
      'provider_probe_failed',
      'A configured provider is broken, unknown, or has no readable version.',
      true,
    ],
    [
      'integration_failed',
      'The Omarchy integration install or its status check did not succeed.',
      true,
    ],
    ['unknown_error', 'Setup stopped for an unrecognized reason.', true],
  ];

  test('there are 21 codes, each with its text and its retry rule', () => {
    expect(TABLE).toHaveLength(21);
    expect(Object.keys(CODE_TEXTS)).toHaveLength(21);
    for (const [code, text, retry] of TABLE) {
      expect(codeInfo(code)).toEqual({ text, retry });
    }
  });

  test('a code outside the table shows as unknown_error', () => {
    expect(codeInfo('<script>')).toEqual(codeInfo('unknown_error'));
    expect(codeInfo('toString')).toEqual(codeInfo('unknown_error'));
  });

  test('the control of a row', () => {
    expect(rowControl(report())).toEqual({ kind: 'allow' });
    expect(rowControl(report({ code: 'release_untrusted' }))).toEqual({ kind: 'disabled' });
    expect(rowControl(report({ retry: { state: 'queued', expires_at: 1 } }))).toEqual({
      kind: 'queued',
    });
    // A retry that exists shows its state, whatever the code.
    expect(
      rowControl(report({ code: 'file_invalid', retry: { state: 'delivered', expires_at: 1 } })),
    ).toEqual({ kind: 'delivered' });
  });
});

describe('the section', () => {
  test('loading, a read error and the hidden case', () => {
    const loading = render({ kind: 'loading' });
    expect(loading).toContain('Setup reports from unpaired keys');
    expect(loading).toContain('Loading setup reports…');
    const error = render({ kind: 'error' });
    expect(error).toContain('role="alert"');
    expect(error).toContain('The setup reports could not be read.');
    expect(render({ kind: 'hidden' })).toBe('');
  });

  test('the section has its heading and its label', () => {
    const html = render(ready([]));
    expect(html).toContain('aria-label="Setup reports from unpaired keys"');
    expect(html).toContain('<h3>Setup reports from unpaired keys</h3>');
  });

  test('the window is closed', () => {
    const html = render(ready([]));
    expect(html).toContain('Reporting is closed. Unpaired keys cannot send setup reports.');
    expect(html).toContain('Open setup reports for one hour');
    expect(html).not.toContain('Close setup reports');
    expect(html).toContain('No setup reports.');
  });

  test('the window is open', () => {
    const deadline = 1_700_003_600_000;
    const html = render(ready([], { open: true, deadline_ms: deadline }));
    expect(html).toContain(`Reporting is open until ${clockText(deadline)}.`);
    expect(html).toContain('Close setup reports');
    expect(html).not.toContain('Open setup reports for one hour');
    expect(html).not.toContain('Reporting is closed');
  });

  test('a row shows the fixed texts and the self-reported values as text', () => {
    const html = render(ready([report({ name: '<b>x</b>' })]));
    expect(html).toContain(
      'This key has not been paired. Its name and setup status are self-reported.',
    );
    expect(html).toContain(`Key: <code class="fd-fingerprint">${groupFingerprint(FP)}</code>`);
    expect(html).toContain('Reported name: &lt;b&gt;x&lt;/b&gt;');
    expect(html).not.toContain('<b>x</b>');
    expect(html).toContain('Pairing did not finish.');
    expect(html).toContain('Phase: pairing');
    expect(html).toContain(`Received: ${clockText(1_700_000_000_000)}`);
    expect(html).toContain(`Last contact: ${clockText(1_700_000_300_000)}`);
    expect(html).toContain('aria-label="Setup report from key 01234567"');
    expect(html).toContain('Dismiss report');
    // A report is never a station, a paired badge or a grant.
    expect(html).not.toContain('retry started');
    expect(html).not.toMatch(/paired badge|Grant|station row/i);
  });

  test('an allowed code shows Allow one setup retry', () => {
    const html = render(ready([report()]));
    expect(html).toContain('Allow one setup retry');
    expect(html).not.toContain('Cancel retry');
    expect(html).not.toContain('Retrying cannot repair this condition.');
  });

  test('a disabled code shows the text and no retry button', () => {
    for (const code of ['config_conflict', 'file_invalid']) {
      const html = render(ready([report({ code })]));
      expect(html).toContain(
        'Retrying cannot repair this condition. Fleet Deck has no board repair for it yet.',
      );
      expect(html).not.toContain('Allow one setup retry');
      expect(html).toContain('Dismiss report');
    }
  });

  test('a queued retry shows its line and Cancel retry', () => {
    const html = render(ready([report({ retry: { state: 'queued', expires_at: 1 } })]));
    expect(html).toContain(
      'Retry allowed. The setup supervisor will check again within about ten minutes while it is running.',
    );
    expect(html).toContain('Cancel retry');
    expect(html).not.toContain('Allow one setup retry');
    expect(html).toContain('Dismiss report');
  });

  test('a retry that exists shows its line whatever the code', () => {
    const html = render(
      ready([report({ code: 'config_conflict', retry: { state: 'queued', expires_at: 1 } })]),
    );
    expect(html).toContain('Retry allowed.');
    expect(html).toContain('Cancel retry');
    expect(html).not.toContain('Retrying cannot repair');
  });

  test('a delivered retry has no cancel', () => {
    const html = render(ready([report({ retry: { state: 'delivered', expires_at: 1 } })]));
    expect(html).toContain('The retry was sent to the station.');
    expect(html).not.toContain('Cancel retry');
    expect(html).not.toContain('Allow one setup retry');
    expect(html).toContain('Dismiss report');
  });

  test('the confirmation is inside the row', () => {
    const html = render(ready([report()]), { confirming: FP });
    expect(html).toContain(
      'Allow this key to try setup once more? This does not pair it, change its trusted deck, replace its identity, or override setup checks.',
    );
    expect(html).toContain('Allow one retry');
    expect(html).toContain('>Cancel<');
    expect(html).not.toContain('Allow one setup retry');
    expect(html).not.toContain('role="dialog"');
    // Another row stays closed.
    expect(render(ready([report()]), { confirming: 'other' })).not.toContain('Allow one retry');
  });

  test('a row error is an alert', () => {
    const html = render(ready([report()]), { errors: { [FP]: 'The deck said no.' } });
    expect(html).toContain('<p role="alert">The deck said no.</p>');
  });
});

describe('the owner actions', () => {
  type Call = { name: string; args: unknown[] };
  const make = (fail?: Error) => {
    const calls: Call[] = [];
    const record = (name: string, args: unknown[]) => {
      calls.push({ name, args });
      if (fail !== undefined) throw fail;
    };
    const actions: ReportsActions = {
      list: async () => {
        throw new Error('not used');
      },
      setWindow: async (...args) => {
        record('setWindow', args);
        return { open: true, deadline_ms: 1 };
      },
      allowRetry: async (...args) => {
        record('allowRetry', args);
        return { state: 'queued', expires_at: 1 };
      },
      cancelRetry: async (...args) => record('cancelRetry', args),
      dismiss: async (...args) => record('dismiss', args),
    };
    return { calls, actions };
  };
  const asks: (readonly string[] | undefined)[] = [];
  const ask =
    (token: string | null): Ask =>
    async (labels) => {
      asks.push(labels);
      return token;
    };

  test('opening asks with the label "Setup reports" and sends the elevation', async () => {
    asks.length = 0;
    const { calls, actions } = make();
    expect(await changeWindow(actions, ask('tok'), true)).toEqual({ kind: 'done' });
    expect(asks).toEqual([['Setup reports']]);
    expect(calls).toEqual([{ name: 'setWindow', args: [true, 'tok'] }]);
  });

  test('a cancelled elevation sends nothing', async () => {
    const { calls, actions } = make();
    expect(await changeWindow(actions, ask(null), true)).toEqual({ kind: 'cancelled' });
    expect(await allowOneRetry(actions, ask(null), new Map(), report())).toEqual({
      kind: 'cancelled',
    });
    expect(calls).toEqual([]);
  });

  test('closing asks for nothing and sends no elevation', async () => {
    asks.length = 0;
    const { calls, actions } = make();
    await changeWindow(actions, ask('tok'), false);
    expect(asks).toEqual([]);
    expect(calls).toEqual([{ name: 'setWindow', args: [false, null] }]);
  });

  test('the deck message of no_tailnet_listener is shown', async () => {
    const { actions } = make(
      new DeckError(409, 'No tailnet link address.', 'no_tailnet_listener', {}),
    );
    expect(await changeWindow(actions, ask('tok'), true)).toEqual({
      kind: 'failed',
      text: 'No tailnet link address.',
      refused: false,
    });
  });

  test('a refused elevation is reported, so that it is forgotten', async () => {
    const { actions } = make(new DeckError(400, 'needs it', 'elevation_required', {}));
    const result = await changeWindow(actions, ask('tok'), true);
    expect(result).toEqual({ kind: 'failed', text: 'needs it', refused: true });
  });

  test('the retry body: the terminal, the digest, a 32-hex request ID and the elevation', async () => {
    const ids = new Map<string, string>();
    const { calls, actions } = make();
    expect(await allowOneRetry(actions, ask('tok'), ids, report())).toEqual({ kind: 'done' });
    // A second click on the same pair sends the same request ID.
    await allowOneRetry(actions, ask('tok'), ids, report());
    expect(calls).toHaveLength(2);
    const [first, second] = calls;
    expect(first?.args.slice(0, 3)).toEqual([FP, TERMINAL, DIGEST]);
    expect(first?.args[3]).toMatch(/^[0-9a-f]{32}$/);
    expect(first?.args[4]).toBe('tok');
    expect(second?.args[3]).toBe(first?.args[3]);
    // Another digest is another pair.
    await allowOneRetry(actions, ask('tok'), ids, report({ state_digest: 'e'.repeat(64) }));
    expect(calls[2]?.args[3]).not.toBe(first?.args[3]);
  });

  test('report_changed shows the fixed text, other errors the deck message', async () => {
    const changed = make(new DeckError(409, 'deck text', 'report_changed', {}));
    expect(await allowOneRetry(changed.actions, ask('tok'), new Map(), report())).toEqual({
      kind: 'failed',
      text: 'The reported setup condition changed. Review the latest report.',
      refused: false,
    });
    const queued = make(
      new DeckError(409, 'A retry is already allowed for this key.', 'retry_queued', {}),
    );
    expect(await allowOneRetry(queued.actions, ask('tok'), new Map(), report())).toEqual({
      kind: 'failed',
      text: 'A retry is already allowed for this key.',
      refused: false,
    });
    expect(retryError('report_changed', 'x')).toBe(
      'The reported setup condition changed. Review the latest report.',
    );
    expect(retryError(null, 'x')).toBe('x');
  });

  test('dismiss and cancel call the deck with the fingerprint and no elevation', async () => {
    const { calls, actions } = make();
    expect(await plainCall(() => actions.dismiss(FP))).toEqual({ kind: 'done' });
    expect(await plainCall(() => actions.cancelRetry(FP))).toEqual({ kind: 'done' });
    expect(calls).toEqual([
      { name: 'dismiss', args: [FP] },
      { name: 'cancelRetry', args: [FP] },
    ]);
    const bad = make(
      new DeckError(409, 'The retry was already delivered to the station.', 'retry_delivered', {}),
    );
    expect(await plainCall(() => bad.actions.cancelRetry(FP))).toEqual({
      kind: 'failed',
      text: 'The retry was already delivered to the station.',
      refused: false,
    });
  });
});

describe('the request IDs', () => {
  test('a new one has 32 hex characters', () => {
    expect(newRequestId()).toMatch(/^[0-9a-f]{32}$/);
    expect(newRequestId()).not.toBe(newRequestId());
  });

  test('one for each pair, kept, and dropped when the pair is gone', () => {
    const ids = new Map<string, string>();
    let n = 0;
    const make = () => `id${++n}`;
    const a = report();
    const b = report({ fingerprint: 'f'.repeat(64) });
    expect(requestIdFor(ids, a, make)).toBe('id1');
    expect(requestIdFor(ids, a, make)).toBe('id1');
    expect(requestIdFor(ids, b, make)).toBe('id2');
    expect(requestIdFor(ids, report({ terminal_id: 'b'.repeat(32) }), make)).toBe('id3');
    pruneRequestIds(ids, [a]);
    expect([...ids.values()]).toEqual(['id1']);
    expect(requestIdFor(ids, b, make)).toBe('id4');
  });
});
