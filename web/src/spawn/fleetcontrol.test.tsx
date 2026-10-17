import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FleetControlFields } from './FleetControlFields';
import {
  answerCapProblem,
  fleetElevation,
  fleetOn,
  grantFields,
  NO_FLEET,
  showFleetFields,
} from './fleetControl';
import { spawnActions } from './spawn';

const PROBLEM = 'The answer limit is a whole number from 0 to 5000.';
const spec = {
  provider_instance: 'claude',
  cwd: '/home/me/app',
  mode: null,
  model: null,
  title: null,
};

describe('grantFields (ADR-008 §3)', () => {
  test('all switches off give exactly the lineage classes', () => {
    expect(grantFields(NO_FLEET)).toEqual({ classes: ['read', 'mail', 'drive'] });
    expect(fleetOn(NO_FLEET)).toBe(false);
  });

  test('each switch adds its class; scope adds scope; answer adds the cap', () => {
    expect(grantFields({ ...NO_FLEET, inspect: true }).classes).toEqual([
      'read',
      'mail',
      'drive',
      'inspect',
    ]);
    expect(grantFields({ ...NO_FLEET, manage: true }).classes).toEqual([
      'read',
      'mail',
      'drive',
      'manage',
    ]);
    expect(grantFields({ ...NO_FLEET, scope: true })).toEqual({
      classes: ['read', 'mail', 'drive'],
      scope: 'fleet',
    });
    expect(grantFields({ ...NO_FLEET, answer: true, answerCap: '12' })).toEqual({
      classes: ['read', 'mail', 'drive', 'answer'],
      answer_cap: 12,
    });
    const all = grantFields({
      scope: true,
      inspect: true,
      manage: true,
      answer: true,
      answerCap: '0',
    });
    expect(all).toEqual({
      classes: ['read', 'mail', 'drive', 'inspect', 'manage', 'answer'],
      scope: 'fleet',
      answer_cap: 0,
    });
    expect(fleetOn({ ...NO_FLEET, manage: true })).toBe(true);
  });
});

describe('answerCapProblem', () => {
  const on = (answerCap: string) => answerCapProblem({ ...NO_FLEET, answer: true, answerCap });
  test('0, 500 and 5000 are fine', () => {
    expect(on('500')).toBeNull();
    expect(on('0')).toBeNull();
    expect(on('5000')).toBeNull();
  });
  test('5001, -1, 1.5, empty and text give the problem', () => {
    for (const bad of ['5001', '-1', '1.5', '', 'abc']) expect(on(bad)).toBe(PROBLEM);
  });
  test('answer off never has a problem', () => {
    expect(answerCapProblem({ ...NO_FLEET, answer: false, answerCap: 'abc' })).toBeNull();
  });
});

describe('showFleetFields', () => {
  test('needs the owner, Coordinator and no import', () => {
    for (const owner of [false, true]) {
      for (const coordinator of [false, true]) {
        for (const importing of [false, true]) {
          expect(showFleetFields(owner, coordinator, importing)).toBe(
            owner && coordinator && !importing,
          );
        }
      }
    }
  });
});

describe('fleetElevation', () => {
  const asker = (answer: string | null) => {
    const calls: (readonly string[] | undefined)[] = [];
    return {
      calls,
      ask: async (labels?: readonly string[]) => {
        calls.push(labels);
        return answer;
      },
    };
  };
  const gate = { coordinator: true, importing: false, fleet: { ...NO_FLEET, inspect: true } };

  test('a switch on asks once with the Fleet control label and returns the code', async () => {
    const a = asker('code-1');
    expect(await fleetElevation(gate, a.ask)).toEqual({ ok: true, elevation: 'code-1' });
    expect(a.calls).toEqual([['Fleet control']]);
  });

  test('a cancelled dialog is not ok', async () => {
    const a = asker(null);
    expect(await fleetElevation(gate, a.ask)).toEqual({ ok: false });
    expect(a.calls.length).toBe(1);
  });

  test('all switches off does not ask', async () => {
    const a = asker('code-1');
    const out = await fleetElevation({ ...gate, fleet: NO_FLEET }, a.ask);
    expect(out).toEqual({ ok: true, elevation: undefined });
    expect(a.calls.length).toBe(0);
  });

  test('an import or no Coordinator does not ask', async () => {
    const a = asker('code-1');
    expect(await fleetElevation({ ...gate, importing: true }, a.ask)).toEqual({
      ok: true,
      elevation: undefined,
    });
    expect(await fleetElevation({ ...gate, coordinator: false }, a.ask)).toEqual({
      ok: true,
      elevation: undefined,
    });
    expect(a.calls.length).toBe(0);
  });
});

describe('spawnActions start with an elevation', () => {
  const run = async (elevation: string | undefined, importId?: string) => {
    const bodies: Record<string, unknown>[] = [];
    const actions = spawnActions(
      async () => ({}),
      async (_path, body) => {
        bodies.push(body as Record<string, unknown>);
        return {
          state: 'done',
          receipt: { state: 'accepted', result: { session_id: 's' }, reject: null },
        };
      },
      ['drive'],
      () => 'c-1',
    );
    await actions.start('st', spec, 'go', null, importId, undefined, undefined, elevation);
    return bodies[0] ?? {};
  };

  test('the body carries the elevation when given', async () => {
    expect((await run('code-1'))['elevation']).toBe('code-1');
  });
  test('the body has no elevation key when not given', async () => {
    expect('elevation' in (await run(undefined))).toBe(false);
  });
  test('an import never sends it', async () => {
    expect('elevation' in (await run('code-1', 'native-1'))).toBe(false);
  });
});

describe('FleetControlFields', () => {
  const html = (fleet = NO_FLEET) =>
    renderToStaticMarkup(<FleetControlFields fleet={fleet} onChange={() => {}} />);

  test('shows the four switches and no answer limit while answer is off', () => {
    const out = html();
    expect(out).toContain('Fleet control');
    expect(out).toContain('Fleet scope: reach every session in the fleet');
    expect(out).toContain('Inspect: read diffs, files, artifacts and integrations');
    expect(out).toContain('Manage: resume, rename, set the mode');
    expect(out).toContain('Answer approvals: allow once or deny approvals');
    expect(out).not.toContain('Answer limit');
    expect(out).not.toContain('confirm it is you');
  });

  test('the answer limit shows with answer on, with the error for 5001', () => {
    const ok = html({ ...NO_FLEET, answer: true });
    expect(ok).toContain('Answer limit');
    expect(ok).toContain('confirm it is you');
    expect(ok).not.toContain(PROBLEM);
    const bad = html({ ...NO_FLEET, answer: true, answerCap: '5001' });
    expect(bad).toContain(PROBLEM);
    expect(bad).toContain('role="alert"');
  });
});
