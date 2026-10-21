// MCP P4b-4 (design P4b §11 test 15): give fleet access and change a live grant on the board.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { GrantView } from '../gen';
import { type SettingsEnv, SettingsProvider } from '../settings/context';
import { FleetControlFields } from '../spawn/FleetControlFields';
import { NO_FLEET } from '../spawn/fleetControl';
import { card, station } from '../testing/fleet';
import type { FleetActions, GrantBody } from './fleet';
import { FleetPanel } from './FleetPanel';
import {
  AccessForm,
  accessBody,
  accessValues,
  ChangeAccess,
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
      />,
    );
    expect(html).toContain('<h4 class="fd-access-h">Stations</h4>');
    expect(html).toContain('Alpha <span class="fd-access-desc">this session</span>');
    expect(html).toContain(
      '<label class="fd-access-row"><input type="checkbox"/><span class="fd-access-title">Beta</span></label>',
    );
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

  test('the form shows the fixed classes disabled and the limits only for new classes', () => {
    const show = (values: ReturnType<typeof accessValues>, codex = false) =>
      renderToStaticMarkup(
        <AccessForm
          grant={grant}
          values={values}
          busy={false}
          codex={codex}
          onChange={() => {}}
          onSave={() => {}}
          onCancel={() => {}}
        />,
      );
    const base = show(accessValues(grant));
    expect(base).toContain('aria-label="Change access"');
    expect(base).toMatch(
      /<input type="checkbox" disabled="" readOnly="" checked=""\/>desktop \(given only at start\)/,
    );
    expect(base).toContain('capture (given only at start)');
    expect(base).not.toContain('input (given only at start)');
    expect(base).not.toContain('Session limit');
    expect(base).not.toContain('Highest Claude mode');
    expect(base).toContain('Save');
    expect(base).toContain('Cancel');
    const more = show({
      ...accessValues(grant),
      classes: [...accessValues(grant).classes, 'answer', 'manage'],
    });
    expect(more).toContain('Answer limit');
    expect(more).toContain('Highest Claude mode');
    expect(more).toContain('<option value="">not allowed</option>');
    expect(more).not.toContain('Session limit');
  });
});

describe('the Codex note', () => {
  const NOTE = 'Codex sees the change when the session resumes.';

  test('the change form shows it for a Codex session only', () => {
    const show = (codex: boolean) =>
      renderToStaticMarkup(
        <AccessForm
          grant={grant}
          values={accessValues(grant)}
          busy={false}
          codex={codex}
          onChange={() => {}}
          onSave={() => {}}
          onCancel={() => {}}
        />,
      );
    expect(show(true)).toContain(NOTE);
    expect(show(false)).not.toContain(NOTE);
  });

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
        />,
      );
    expect(html('codex')).toContain(NOTE);
    expect(html('claude')).not.toContain(NOTE);
  });
});

describe('the Change access button', () => {
  const button = (g: GrantView, ask: (() => Promise<string | null>) | undefined) =>
    renderToStaticMarkup(
      <ChangeAccess grant={g} actions={actions()} ask={ask} codex={false} onChanged={() => {}} />,
    );

  test('shows for a live grant with an ask, and not for a revoked, a non-live or no-ask grant', () => {
    expect(button(grant, async () => null)).toContain('Change access');
    expect(button({ ...grant, state: 'revoked' }, async () => null)).not.toContain('Change access');
    expect(button({ ...grant, live: false }, async () => null)).not.toContain('Change access');
    expect(button(grant, undefined)).not.toContain('Change access');
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

  test('GiveAccess shows for the owner with a coordinator_live station', () => {
    expect(render([live], true)).toContain('aria-label="Give fleet access"');
  });

  test('GiveAccess is not rendered without a coordinator_live station, or for a login that is not the owner', () => {
    expect(render([old], true)).not.toContain('Give fleet access');
    expect(render([station('station-a')], true)).not.toContain('Give fleet access');
    expect(render([live], false)).not.toContain('Give fleet access');
  });
});
