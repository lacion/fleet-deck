// MCP P4b-4 (design P4b §11 test 15): give fleet access and change a live grant on the board.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { GrantView } from '../gen';
import { type SettingsEnv, SettingsProvider } from '../settings/context';
import { FleetControlFields } from '../spawn/FleetControlFields';
import { NO_FLEET } from '../spawn/fleetControl';
import { accessRows, FleetAccess, grantRowText, grantState } from './FleetAccess';
import { card, station } from '../testing/fleet';
import type { FleetActions, GrantBody } from './fleet';
import { FleetPanel } from './FleetPanel';
import {
  accessBody,
  accessValues,
  GiveAccess,
  giveAccess,
  giveCandidates,
  liveGrantFields,
  saveAccess,
  START_LIVE,
} from './GrantAccess';

const CODEX = 'on-request/workspace-write';

const grant: GrantView = {
  grant_id: 'g',
  station_id: 'station-a',
  session_id: 'coord-1',
  classes: ['read', 'mail', 'drive', 'spawn', 'desktop', 'capture'],
  stations: ['station-a'],
  spawn_cap: 3,
  spawn_used: 0,
  state: 'active',
  revoked_reason: null,
  generation: 1,
  expires_at: 10,
  last_used_at: null,
  max_modes: { claude: 'acceptEdits', codex: CODEX },
  live: true,
};

type Give = { station: string; session: string; body: GrantBody; elevation: string | undefined };
type Change = { grantId: string; body: object; elevation: string | undefined };

const actions = (): FleetActions & { readonly gives: Give[]; readonly changes: Change[] } => {
  const gives: Give[] = [];
  const changes: Change[] = [];
  return {
    gives,
    changes,
    mail: async () => ({ ok: true, text: '' }),
    task: async () => ({ ok: true, text: '' }),
    tasks: async () => [],
    grants: async () => [],
    revoke: async () => ({ ok: true, text: '' }),
    renew: async () => ({ ok: true, text: '' }),
    caps: async () => ({ ok: true, text: '' }),
    desktopOps: async () => [],
    give: async (station, session, body, elevation) => {
      gives.push({ station, session, body, elevation });
      return { ok: true, text: 'Fleet access given.' };
    },
    changeAccess: async (grantId, body, elevation) => {
      changes.push({ grantId, body, elevation });
      return { ok: true, text: 'Access changed.' };
    },
  };
};

/** A fake `ask` that records its labels and answers `code`. */
const asker = (code: string | null) => {
  const labels: (readonly string[])[] = [];
  return {
    labels,
    ask: async (l?: readonly string[]) => {
      labels.push(l ?? []);
      return code;
    },
  };
};

const live = station('station-a', { coordinator_live: true });
const old = station('station-b', { coordinator_live: false });

describe('the session select', () => {
  const sessions = [
    card('one', 'working', 'station-a', { title: 'Fix the build' }),
    card('two', 'working', 'station-a'),
    card('three', 'working', 'station-b'),
    card('four', 'ended', 'station-a'),
    card('five', 'lost', 'station-a'),
    card('six', 'unreachable', 'station-a'),
    card('seven', 'working', 'station-a', { hibernated: true }),
  ];

  test('lists only reachable sessions of coordinator_live stations without a live grant', () => {
    const grants: GrantView[] = [
      { ...grant, session_id: 'two' },
      { ...grant, grant_id: 'h', session_id: 'seven', live: false },
    ];
    const ids = giveCandidates(sessions, [live, old], grants).map((s) => s.summary.session_id);
    expect(ids).toEqual(['one', 'seven']);
  });

  test('an expired live grant hides the session; a revoked one does not', () => {
    const expired: GrantView = { ...grant, session_id: 'one', state: 'expired' };
    const revoked: GrantView = { ...grant, session_id: 'two', state: 'revoked' };
    const ids = giveCandidates(sessions, [live], [expired, revoked]).map(
      (s) => s.summary.session_id,
    );
    expect(ids).toEqual(['two', 'seven']);
  });

  test('the options say the title or the id, and the station name', () => {
    const html = renderToStaticMarkup(
      <GiveAccess
        sessions={sessions}
        stations={[live, old]}
        grants={[]}
        actions={actions()}
        ask={async () => null}
        onChanged={() => {}}
        onClose={() => {}}
      />,
    );
    expect(html).toContain('aria-label="Give fleet access"');
    expect(html).toContain('Give fleet access');
    expect(html).toContain('Session');
    expect(html).toContain('>Fix the build on Alpha</option>');
    expect(html).toContain('>two on Alpha</option>');
    expect(html).not.toContain('three on');
    expect(html).not.toContain('four on');
    expect(html).toContain('Give access');
  });

  test('the give form lists the stations: the own one fixed, the others to pick', () => {
    const html = renderToStaticMarkup(
      <GiveAccess
        sessions={sessions}
        stations={[live, old]}
        grants={[]}
        actions={actions()}
        ask={async () => null}
        onChanged={() => {}}
        onClose={() => {}}
      />,
    );
    // The form of the session menu's dialog (AccessFields): .fd-label sub-headings, switch rows.
    expect(html).toContain('<h4 class="fd-label fd-access-h">Stations</h4>');
    expect(html).toContain(
      '<span class="fd-access-title">Alpha</span><span class="fd-access-desc">The station of this session: always in the access.</span>',
    );
    expect(html).toContain(
      '<label class="fd-access-row"><span class="fd-access-text"><span class="fd-access-title">Beta</span></span><input type="checkbox" role="switch" class="fd-switch" aria-checked="false"/></label>',
    );
    // Not the spawn form's fleet control fields: no framed boxes.
    expect(html).not.toContain('<fieldset');
    expect(html).not.toContain('Sessions it may start');
    expect(html).not.toContain('Allow starting sessions');
    expect(html).toContain('<span class="fd-access-title">Start sessions</span>');
  });

  test('with no candidate it shows the note and no form', () => {
    const html = renderToStaticMarkup(
      <GiveAccess
        sessions={[card('three', 'working', 'station-b')]}
        stations={[live, old]}
        grants={[]}
        actions={actions()}
        ask={async () => null}
        onChanged={() => {}}
        onClose={() => {}}
      />,
    );
    expect(html).toContain('No running session can receive fleet access.');
    expect(html).not.toContain('<form');
    expect(html).not.toContain('Give access');
  });
});

describe('the live form', () => {
  test('hides the desktop, screenshot, recording, input and click switches', () => {
    const full = { ...NO_FLEET, desktop: true, capture: true, recording: true, input: true };
    const html = renderToStaticMarkup(<FleetControlFields fleet={full} onChange={() => {}} live />);
    for (const text of [
      'Desktop:',
      'Screenshots:',
      'Allow monitor recordings',
      'Allow keyboard input',
      'Allow pointer clicks',
      'Desktop action limit',
    ])
      expect(html).not.toContain(text);
    expect(html).toContain('Allow starting sessions');
    expect(html).toContain('Inspect:');
    const normal = renderToStaticMarkup(
      <FleetControlFields fleet={NO_FLEET} onChange={() => {}} />,
    );
    expect(normal).toContain('Desktop:');
    expect(normal).toContain('Screenshots:');
  });

  test('liveGrantFields sends the stations only when targets are given', () => {
    expect(liveGrantFields(START_LIVE, ['b']).stations).toEqual(['b']);
    expect(liveGrantFields(START_LIVE, [])).not.toHaveProperty('stations');
    expect(liveGrantFields(START_LIVE)).not.toHaveProperty('stations');
  });

  test('liveGrantFields has no desktop field, whatever the form holds', () => {
    const full = { ...NO_FLEET, spawn: true, desktop: true, capture: true, input: true };
    const body = liveGrantFields(full);
    expect(body).toEqual({
      classes: ['read', 'mail', 'drive', 'spawn'],
      spawn_cap: 3,
      max_modes: { claude: 'acceptEdits', codex: CODEX },
    });
    expect(liveGrantFields({ ...START_LIVE, answer: true, scope: true, inspect: true })).toEqual({
      classes: ['read', 'mail', 'drive', 'spawn', 'inspect', 'answer'],
      spawn_cap: 3,
      max_modes: { claude: 'acceptEdits', codex: CODEX },
      scope: 'fleet',
      answer_cap: 500,
    });
  });
});

describe('give', () => {
  const session = card('one', 'working', 'station-a');

  test('sends liveGrantFields to the station and session with the elevation code', async () => {
    const a = actions();
    const k = asker('123456');
    const r = await giveAccess(session, START_LIVE, a, k.ask);
    expect(r?.ok).toBe(true);
    expect(k.labels).toEqual([['Starting sessions']]);
    expect(a.gives).toEqual([
      {
        station: 'station-a',
        session: 'one',
        body: liveGrantFields(START_LIVE),
        elevation: '123456',
      },
    ]);
  });

  test('fleet control asks with the Fleet control label', async () => {
    const a = actions();
    const k = asker('c');
    await giveAccess(session, { ...START_LIVE, inspect: true }, a, k.ask);
    expect(k.labels).toEqual([['Fleet control']]);
  });

  test('a cancelled elevation sends nothing', async () => {
    const a = actions();
    const k = asker(null);
    expect(await giveAccess(session, START_LIVE, a, k.ask)).toBeNull();
    expect(k.labels).toEqual([['Starting sessions']]);
    expect(a.gives).toEqual([]);
  });

  test('targets are sent as stations and ask the Fleet control label', async () => {
    const a = actions();
    const k = asker('9');
    await giveAccess(session, NO_FLEET, a, k.ask, ['station-b']);
    expect(k.labels).toEqual([['Fleet control']]);
    expect(a.gives[0]?.body.stations).toEqual(['station-b']);
    expect(a.gives[0]?.elevation).toBe('9');
  });

  test('a form with nothing on asks nothing and sends no elevation', async () => {
    const a = actions();
    const k = asker('c');
    await giveAccess(session, NO_FLEET, a, k.ask);
    expect(k.labels).toEqual([]);
    expect(a.gives[0]?.elevation).toBeUndefined();
    expect(a.gives[0]?.body.classes).toEqual(['read', 'mail', 'drive']);
  });
});

describe('change', () => {
  test('removing a class sends no elevation and keeps the fixed classes', async () => {
    const a = actions();
    const k = asker('c');
    const v = { ...accessValues(grant), classes: ['read', 'mail', 'drive'] };
    const r = await saveAccess(grant, v, a, k.ask);
    expect(r?.ok).toBe(true);
    expect(k.labels).toEqual([]);
    expect(a.changes).toEqual([
      {
        grantId: 'g',
        body: { classes: ['read', 'mail', 'drive', 'desktop', 'capture'] },
        elevation: undefined,
      },
    ]);
  });

  test('adding a class asks Fleet access; a cancel sends nothing; a code is sent', async () => {
    const v = { ...accessValues(grant), classes: [...accessValues(grant).classes, 'inspect'] };
    const a = actions();
    const cancel = asker(null);
    expect(await saveAccess(grant, v, a, cancel.ask)).toBeNull();
    expect(cancel.labels).toEqual([['Fleet access']]);
    expect(a.changes).toEqual([]);
    const ok = asker('777777');
    await saveAccess(grant, v, a, ok.ask);
    expect(ok.labels).toEqual([['Fleet access']]);
    expect(a.changes).toEqual([
      {
        grantId: 'g',
        body: { classes: ['read', 'mail', 'drive', 'spawn', 'inspect', 'desktop', 'capture'] },
        elevation: '777777',
      },
    ]);
  });

  test('a newly added spawn sends spawn_cap and max_modes', () => {
    const g: GrantView = { ...grant, classes: ['read', 'mail', 'drive'], max_modes: {} };
    const v = { ...accessValues(g), classes: ['read', 'mail', 'drive', 'spawn'], spawnCap: '5' };
    expect(accessBody(g, v)).toEqual({
      classes: ['read', 'mail', 'drive', 'spawn'],
      spawn_cap: 5,
      max_modes: { claude: 'acceptEdits', codex: CODEX },
    });
    const none = { ...v, claude: '' };
    expect(accessBody(g, none).max_modes).toEqual({ codex: CODEX });
  });

  test('a newly added manage sends max_modes and no spawn_cap; answer sends answer_cap', () => {
    const g: GrantView = { ...grant, classes: ['read'], max_modes: { claude: 'plan' } };
    const v = { ...accessValues(g), classes: ['read', 'manage', 'answer'], answerCap: '40' };
    expect(accessBody(g, v)).toEqual({
      classes: ['read', 'manage', 'answer'],
      max_modes: { claude: 'plan', codex: CODEX },
      answer_cap: 40,
    });
  });

  test('a class that stays sends no limit', () => {
    const v = { ...accessValues(grant), spawnCap: '9', answerCap: '9' };
    expect(accessBody(grant, v)).toEqual({
      classes: ['read', 'mail', 'drive', 'spawn', 'desktop', 'capture'],
    });
  });
});

describe('the Codex note', () => {
  const NOTE = 'Codex sees the change when the session resumes.';

  test('the give form shows it when the chosen session is a Codex session', () => {
    const html = (provider: 'codex' | 'claude') =>
      renderToStaticMarkup(
        <GiveAccess
          sessions={[card('one', 'working', 'station-a', { provider })]}
          stations={[live]}
          grants={[]}
          actions={actions()}
          ask={async () => null}
          onChanged={() => {}}
          onClose={() => {}}
        />,
      );
    expect(html('codex')).toContain(NOTE);
    expect(html('claude')).not.toContain(NOTE);
  });
});

describe('the fleet panel', () => {
  const env = {
    settings: {} as never,
    accounts: {} as never,
    tailnet: {} as never,
    displays: {} as never,
    desktop: {} as never,
    capture: {} as never,
    deletion: {} as never,
  } as SettingsEnv;
  const render = (stations: ReturnType<typeof station>[], owner: boolean) => {
    const panel = <FleetPanel actions={actions()} sessions={[card('a')]} stations={stations} />;
    return renderToStaticMarkup(
      owner ? <SettingsProvider env={env}>{panel}</SettingsProvider> : panel,
    );
  };

  test('Give fleet access… shows for the owner with a coordinator_live station', () => {
    const html = render([live], true);
    expect(html).toContain('aria-label="Fleet access"');
    expect(html).toContain('</svg> Give fleet access…</button>');
    // The form is in a dialog: the panel shows only the button.
    expect(html).not.toContain('aria-label="Give fleet access"');
    expect(html).not.toContain('fd-switch');
  });

  test('Give fleet access… is not rendered without a coordinator_live station, or for a login that is not the owner', () => {
    expect(render([old], true)).not.toContain('Give fleet access');
    expect(render([station('station-a')], true)).not.toContain('Give fleet access');
    expect(render([live], false)).not.toContain('Give fleet access');
  });
});

describe('the fleet access rows', () => {
  const rows = (grants: readonly GrantView[], owner = true) =>
    renderToStaticMarkup(
      <SettingsProvider env={owner ? ({} as SettingsEnv) : (undefined as never)}>
        <FleetAccess
          sessions={[card('coord-1', 'working', 'station-a', { title: 'Plan the release' })]}
          stations={[live]}
          grants={grants}
          actions={actions()}
          layout="section"
          onChanged={() => {}}
        />
      </SettingsProvider>,
    );

  test('lists the grants that are not revoked, with the count in the heading', () => {
    expect(accessRows([grant, { ...grant, grant_id: 'r', state: 'revoked' }])).toEqual([grant]);
    const html = rows([grant, { ...grant, grant_id: 'r', state: 'revoked' }]);
    expect(html).toContain('<section class="fd-ops-sec fd-ops-access" aria-label="Fleet access">');
    expect(html).toMatch(
      /<h2 class="fd-ops-h"><svg[\s\S]*?<\/svg> Fleet access<span class="fd-count">1<\/span><\/h2>/,
    );
    expect(html).toContain('aria-label="Coordinator grants"');
    expect(html).toContain(
      '<span class="fd-ops-row-title" id="fd-grant-g">Plan the release</span>',
    );
    // A short row: the kind and the state; the classes and limits are its title and its dialog.
    expect(html).toContain(
      '<span class="fd-ops-row-sub" data-state="active">Coordinator · active</span>',
    );
    expect(html).toContain(
      'title="read, mail, drive, spawn, desktop, capture, spawned 0 of 3: active"',
    );
  });

  test('the rows: active grants first; the state says waiting, active, expired after idle time', () => {
    const waiting: GrantView = { ...grant, grant_id: 'w', generation: null };
    const idle: GrantView = { ...grant, grant_id: 'i', state: 'expired', revoked_reason: 'idle' };
    expect(accessRows([idle, grant, waiting]).map((g) => g.grant_id)).toEqual(['g', 'w', 'i']);
    expect(grantState(waiting)).toBe('waiting for its session');
    expect(grantState(idle)).toBe('expired after idle time');
    expect(grantState({ ...idle, revoked_reason: 'ttl' })).toBe('expired');
  });

  test('a_grant_ended_by_a_station_change_says_why', () => {
    const ended = (reason: string): GrantView => ({
      ...grant,
      state: 'revoked',
      revoked_reason: reason,
    });
    expect(grantState(ended('station_updated'))).toBe('ended by a station update');
    expect(grantState(ended('station_reset'))).toBe('ended: the station has new data');
    expect(grantState(ended('station_removed'))).toBe('ended: the station was removed');
    expect(grantState(ended('owner'))).toBe('revoked');
  });

  test('the expired grants fold under a collapsed Expired row; the heading counts the others', () => {
    const idle: GrantView = { ...grant, grant_id: 'i', state: 'expired', revoked_reason: 'idle' };
    const html = rows([grant, idle]);
    expect(html).toMatch(
      /<h2 class="fd-ops-h"><svg[\s\S]*?<\/svg> Fleet access<span class="fd-count">1<\/span><\/h2>/,
    );
    expect(html).toContain(
      '<details class="fd-access-expired"><summary>Expired <span class="fd-count">1</span></summary>',
    );
    expect(html).toMatch(/<details class="fd-access-expired">[\s\S]*aria-label="Expired grants"/);
    expect(html).toContain(
      '<span class="fd-ops-row-sub" data-state="expired">Coordinator · expired after idle time</span>',
    );
    // Only expired grants: no count in the heading, the fold holds them.
    const only = rows([idle]);
    expect(only).not.toContain('aria-label="Coordinator grants"');
    expect(only).toMatch(/<\/svg> Fleet access<\/h2>/);
    expect(rows([grant])).not.toContain('fd-access-expired');
  });

  test('a row opens the session dialog with Change…; no inline Revoke, Renew or form', () => {
    const html = rows([grant]);
    expect(html).toContain('aria-describedby="fd-grant-g">Change…</button>');
    for (const text of ['Revoke', 'Renew', 'Change access', 'Change session rules', '<form'])
      expect(html).not.toContain(text);
  });

  test('a grant of a session that is not on the board has its row and Change… too', () => {
    const html = rows([{ ...grant, session_id: 'gone' }]);
    // The kind and the station name the row; the line under it says the state once.
    expect(html).toContain(
      '<span class="fd-ops-row-title" id="fd-grant-g">Coordinator on Alpha</span>',
    );
    expect(html).toContain('<span class="fd-ops-row-sub" data-state="active">Active</span>');
    expect(html).toContain('aria-describedby="fd-grant-g">Change…</button>');
    const waiting = rows([{ ...grant, session_id: '', generation: null }]);
    expect(waiting).toContain('>Coordinator on Alpha</span>');
    expect(waiting).toContain(
      '<span class="fd-ops-row-sub" data-state="active">Waiting for its session</span>',
    );
    expect(waiting).not.toContain('Coordinator ·');
    const nowhere = grantRowText({ ...grant, station_id: '', kind: 'worker' }, undefined, []);
    expect(nowhere).toEqual({ title: 'New worker', sub: 'Active' });
  });

  test('no grant and nothing to give: no section', () => {
    expect(rows([{ ...grant, state: 'revoked' }], false)).toBe('');
  });
});
