// MCP P4a §8: the session rules form of a grant (static parts, the body, and the elevation asks).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { GrantView } from '../gen';
import type { FleetActions } from './fleet';
import {
  GrantSpawn,
  grantTitle,
  hasCeilings,
  modeWithin,
  NOT_ALLOWED,
  SpawnForm,
  saveSpawn,
  sessionLimitProblem,
  spawnBody,
  spawnValues,
} from './GrantSpawn';

const CODEX = 'on-request/workspace-write';

const grant: GrantView = {
  grant_id: 'g',
  station_id: 'station-a',
  session_id: 'coord-1',
  classes: ['read', 'mail', 'drive', 'spawn'],
  stations: ['station-a'],
  spawn_cap: 3,
  spawn_used: 0,
  state: 'active',
  revoked_reason: null,
  generation: 1,
  expires_at: 10,
  last_used_at: null,
  max_modes: { claude: 'acceptEdits', codex: CODEX },
};

type Call = { grantId: string; caps: object; elevation: string | undefined };

const actions = (): FleetActions & { readonly calls: Call[] } => {
  const calls: Call[] = [];
  return {
    calls,
    mail: async () => ({ ok: true, text: '' }),
    task: async () => ({ ok: true, text: '' }),
    tasks: async () => [],
    grants: async () => [],
    revoke: async () => ({ ok: true, text: '' }),
    renew: async () => ({ ok: true, text: '' }),
    desktopOps: async () => [],
    give: async () => ({ ok: true, text: '' }),
    changeAccess: async () => ({ ok: true, text: '' }),
    caps: async (grantId, caps, elevation) => {
      calls.push({ grantId, caps, elevation });
      return { ok: true, text: 'Limits changed.' };
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

const show = (g: GrantView) =>
  renderToStaticMarkup(
    <SpawnForm
      grant={g}
      values={spawnValues(g)}
      busy={false}
      onChange={() => {}}
      onSave={() => {}}
      onCancel={() => {}}
    />,
  );

describe('the session rules form', () => {
  test('a spawn grant shows the limit and both selects with the values selected', () => {
    const html = show(grant);
    expect(html).toContain('aria-label="Change session rules"');
    expect(html).toContain('Session limit');
    expect(html).toContain('value="3"');
    expect(html).toContain('Highest Claude mode');
    expect(html).toContain('Highest Codex mode');
    expect(html).toContain('<option value="acceptEdits" selected="">acceptEdits</option>');
    expect(html).toContain(`<option value="${CODEX}" selected="">${CODEX}</option>`);
    expect(html).toContain('Sessions that it starts, resumes or changes never get a higher mode.');
    expect(html).toContain('Save');
    expect(html).toContain('Cancel');
  });

  test('a manage-only grant shows the selects and no session limit', () => {
    const g: GrantView = { ...grant, classes: ['read', 'manage'] };
    const html = show(g);
    expect(html).not.toContain('Session limit');
    expect(html).toContain('Highest Claude mode');
    expect(html).toContain('Highest Codex mode');
  });

  test('a family without a ceiling shows "not allowed" selected', () => {
    const g: GrantView = { ...grant, max_modes: { claude: 'plan' } };
    expect(spawnValues(g).codex).toBe(NOT_ALLOWED);
    const html = show(g);
    expect(html).toContain('<option value="" selected="">not allowed</option>');
    expect(html).toContain('<option value="plan" selected="">plan</option>');
  });

  test('a grant with neither class has no ceilings and no button', () => {
    const g: GrantView = { ...grant, classes: ['read', 'mail'] };
    expect(hasCeilings(g)).toBe(false);
    expect(hasCeilings(grant)).toBe(true);
    const html = renderToStaticMarkup(
      <GrantSpawn grant={grant} actions={actions()} ask={undefined} onChanged={() => {}} />,
    );
    expect(html).not.toContain('Change session rules');
    const withAsk = renderToStaticMarkup(
      <GrantSpawn grant={grant} actions={actions()} ask={async () => null} onChanged={() => {}} />,
    );
    expect(withAsk).toContain('Change session rules');
    const revoked = renderToStaticMarkup(
      <GrantSpawn
        grant={{ ...grant, state: 'revoked' }}
        actions={actions()}
        ask={async () => null}
        onChanged={() => {}}
      />,
    );
    expect(revoked).not.toContain('Change session rules');
  });

  test('a worker grant: 11 is a problem with Save disabled, and the row title says Worker', () => {
    const w: GrantView = { ...grant, kind: 'worker' };
    const v = { ...spawnValues(w), spawnCap: '11' };
    expect(sessionLimitProblem(w, v)).toBe('The session limit is a whole number from 0 to 10.');
    expect(sessionLimitProblem(w, { ...v, spawnCap: '10' })).toBeNull();
    expect(sessionLimitProblem(grant, { ...v, spawnCap: '11' })).toBeNull();
    expect(sessionLimitProblem(grant, { ...v, spawnCap: '51' })).toBe(
      'The session limit is a whole number from 0 to 50.',
    );
    expect(sessionLimitProblem(grant, { ...v, spawnCap: 'x' })).not.toBeNull();
    const html = renderToStaticMarkup(
      <SpawnForm
        grant={w}
        values={v}
        busy={false}
        onChange={() => {}}
        onSave={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(html).toContain('The session limit is a whole number from 0 to 10.');
    expect(html).toMatch(/<button type="submit" disabled="">Save<\/button>/);
    expect(grantTitle({ ...w, session_id: 'coord-1' })).toBe('Worker coord-1');
    expect(grantTitle(grant)).toBe('Coordinator coord-1');
  });
});

describe('the body and the saves', () => {
  test('the limit lowered sends only spawn_cap, with no elevation and no ask', async () => {
    const a = actions();
    const k = asker('code');
    const r = await saveSpawn(grant, { ...spawnValues(grant), spawnCap: '1' }, a, k.ask);
    expect(r?.ok).toBe(true);
    expect(a.calls).toEqual([{ grantId: 'g', caps: { spawn_cap: 1 }, elevation: undefined }]);
    expect(k.labels).toEqual([]);
  });

  test('Claude lowered sends the full map, with no ask', async () => {
    const a = actions();
    const k = asker('code');
    await saveSpawn(grant, { ...spawnValues(grant), claude: 'plan' }, a, k.ask);
    expect(a.calls).toEqual([
      {
        grantId: 'g',
        caps: { max_modes: { claude: 'plan', codex: CODEX } },
        elevation: undefined,
      },
    ]);
    expect(k.labels).toEqual([]);
  });

  test('the limit raised asks the elevation; a cancel sends nothing; a code is sent', async () => {
    const v = { ...spawnValues(grant), spawnCap: '4' };
    const a = actions();
    const cancel = asker(null);
    expect(await saveSpawn(grant, v, a, cancel.ask)).toBeNull();
    expect(cancel.labels).toEqual([['Session limits']]);
    expect(a.calls).toEqual([]);
    const ok = asker('123456');
    const r = await saveSpawn(grant, v, a, ok.ask);
    expect(r?.ok).toBe(true);
    expect(ok.labels).toEqual([['Session limits']]);
    expect(a.calls).toEqual([{ grantId: 'g', caps: { spawn_cap: 4 }, elevation: '123456' }]);
  });

  test('an incomparable Codex mode asks; a new family asks; removing a family does not ask', async () => {
    const a = actions();
    const k = asker('c');
    await saveSpawn(grant, { ...spawnValues(grant), codex: 'never/read-only' }, a, k.ask);
    expect(k.labels).toEqual([['Session limits']]);
    expect(a.calls[0]?.elevation).toBe('c');

    const g: GrantView = { ...grant, max_modes: { codex: CODEX } };
    const b = actions();
    const k2 = asker('c');
    await saveSpawn(g, { ...spawnValues(g), claude: 'plan' }, b, k2.ask);
    expect(k2.labels).toEqual([['Session limits']]);
    expect(b.calls[0]?.caps).toEqual({ max_modes: { claude: 'plan', codex: CODEX } });

    const c = actions();
    const k3 = asker('c');
    await saveSpawn(grant, { ...spawnValues(grant), codex: NOT_ALLOWED }, c, k3.ask);
    expect(k3.labels).toEqual([]);
    expect(c.calls).toEqual([
      { grantId: 'g', caps: { max_modes: { claude: 'acceptEdits' } }, elevation: undefined },
    ]);
    expect(c.calls[0]?.caps).not.toHaveProperty('max_modes.codex');
  });

  test('no change answers "Nothing changed." and sends nothing', async () => {
    const a = actions();
    const k = asker('code');
    expect(await saveSpawn(grant, spawnValues(grant), a, k.ask)).toEqual({
      ok: true,
      text: 'Nothing changed.',
    });
    expect(a.calls).toEqual([]);
    expect(k.labels).toEqual([]);
    expect(spawnBody(grant, spawnValues(grant))).toEqual({});
  });

  test('a manage-only grant never sends spawn_cap', () => {
    const g: GrantView = { ...grant, classes: ['read', 'manage'] };
    expect(spawnBody(g, { ...spawnValues(g), spawnCap: '9', claude: 'plan' })).toEqual({
      max_modes: { claude: 'plan', codex: CODEX },
    });
  });
});

describe('modeWithin', () => {
  test('Claude order', () => {
    expect(modeWithin('claude', 'plan', 'acceptEdits')).toBe(true);
    expect(modeWithin('claude', 'acceptEdits', 'acceptEdits')).toBe(true);
    expect(modeWithin('claude', 'auto', 'acceptEdits')).toBe(false);
  });

  test('Codex needs both the policy and the sandbox to be within', () => {
    expect(modeWithin('codex', 'on-request/read-only', CODEX)).toBe(true);
    expect(modeWithin('codex', CODEX, CODEX)).toBe(true);
    expect(modeWithin('codex', 'never/read-only', CODEX)).toBe(false);
    expect(modeWithin('codex', CODEX, 'never/read-only')).toBe(false);
  });

  test('an unknown value is false', () => {
    expect(modeWithin('claude', 'bogus', 'auto')).toBe(false);
    expect(modeWithin('claude', 'plan', 'bogus')).toBe(false);
    expect(modeWithin('codex', 'bogus', CODEX)).toBe(false);
    expect(modeWithin('codex', 'on-request/bogus', CODEX)).toBe(false);
    expect(modeWithin('codex', 'plan', CODEX)).toBe(false);
  });
});
