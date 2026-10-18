import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FleetControlFields } from './FleetControlFields';
import {
  answerCapProblem,
  captureCapProblem,
  desktopCapProblem,
  fleetElevation,
  fleetOn,
  grantFields,
  imageBudgetProblem,
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
      ...NO_FLEET,
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

describe('grantFields with desktop and capture (R13 P6)', () => {
  test('the defaults are 300, 20 and 64 MiB', () => {
    expect(NO_FLEET.desktop).toBe(false);
    expect(NO_FLEET.capture).toBe(false);
    expect(NO_FLEET.desktopCap).toBe('300');
    expect(NO_FLEET.captureCap).toBe('20');
    expect(NO_FLEET.imageMib).toBe('64');
    expect(fleetOn({ ...NO_FLEET, desktop: true })).toBe(true);
    expect(fleetOn({ ...NO_FLEET, capture: true })).toBe(true);
  });
  test('desktop adds its class and desktop_cap only', () => {
    expect(grantFields({ ...NO_FLEET, desktop: true, desktopCap: '12' })).toEqual({
      classes: ['read', 'mail', 'drive', 'desktop'],
      desktop_cap: 12,
    });
  });
  test('capture adds its class and all three limits, the image bytes in MiB x 1,048,576', () => {
    expect(grantFields({ ...NO_FLEET, capture: true })).toEqual({
      classes: ['read', 'mail', 'drive', 'capture'],
      desktop_cap: 300,
      capture_cap: 20,
      image_byte_cap: 67_108_864,
    });
  });
  test('desktop and capture together keep one desktop_cap', () => {
    expect(
      grantFields({ ...NO_FLEET, desktop: true, capture: true, desktopCap: '7', imageMib: '1' }),
    ).toEqual({
      classes: ['read', 'mail', 'drive', 'desktop', 'capture'],
      desktop_cap: 7,
      capture_cap: 20,
      image_byte_cap: 1_048_576,
    });
  });
});

describe('the desktop limit problems', () => {
  const D = 'The desktop action limit is a whole number from 0 to 5000.';
  const C = 'The screenshot limit is a whole number from 0 to 5000.';
  const I = 'The image read budget is a whole number of MiB from 0 to 1024.';
  test('desktopCapProblem is checked for desktop or capture only', () => {
    expect(desktopCapProblem({ ...NO_FLEET, desktopCap: 'x' })).toBeNull();
    for (const key of ['desktop', 'capture'] as const) {
      const on = (desktopCap: string) =>
        desktopCapProblem({ ...NO_FLEET, [key]: true, desktopCap });
      expect(on('0')).toBeNull();
      expect(on('5000')).toBeNull();
      for (const bad of ['5001', '-1', '1.5', '', 'abc']) expect(on(bad)).toBe(D);
    }
  });
  test('captureCapProblem is checked for capture only', () => {
    expect(captureCapProblem({ ...NO_FLEET, desktop: true, captureCap: 'x' })).toBeNull();
    const on = (captureCap: string) =>
      captureCapProblem({ ...NO_FLEET, capture: true, captureCap });
    expect(on('0')).toBeNull();
    expect(on('5000')).toBeNull();
    for (const bad of ['5001', '-1', '1.5', '', 'abc']) expect(on(bad)).toBe(C);
  });
  test('imageBudgetProblem is checked for capture only, 0 to 1024', () => {
    expect(imageBudgetProblem({ ...NO_FLEET, desktop: true, imageMib: 'x' })).toBeNull();
    const on = (imageMib: string) => imageBudgetProblem({ ...NO_FLEET, capture: true, imageMib });
    expect(on('0')).toBeNull();
    expect(on('1024')).toBeNull();
    for (const bad of ['1025', '-1', '1.5', '', 'abc']) expect(on(bad)).toBe(I);
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

  test('a desktop-only or capture-only start asks once with the Fleet control label', async () => {
    for (const fleet of [
      { ...NO_FLEET, desktop: true },
      { ...NO_FLEET, capture: true },
    ]) {
      const a = asker('code-2');
      expect(await fleetElevation({ ...gate, fleet }, a.ask)).toEqual({
        ok: true,
        elevation: 'code-2',
      });
      expect(a.calls).toEqual([['Fleet control']]);
    }
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

  const DESKTOP =
    'Desktop: show notifications and previews on grant stations, close its own previews, and show its work on walls';
  const CAPTURE =
    'Screenshots: capture a whole monitor of a grant station and read its own screenshots';
  const DESKTOP_NOTE =
    'Opening a preview runs web content as your user on that station, and the page can make network requests.';
  const CAPTURE_NOTE =
    'This agent can capture the whole selected monitor, including other applications, notifications and private information visible there. Screenshots are stored as artifacts. Images the agent reads are sent to its provider conversation. Deleting an artifact does not remove downloaded copies or provider conversations.';
  const GATE =
    'Desktop and screenshot tools work only while the deck settings Fleet control and Desktop control are on, and on stations that allow them.';

  test('the desktop and screenshot switches show with their texts and nothing else while off', () => {
    const out = html();
    expect(out).toContain(DESKTOP);
    expect(out).toContain(CAPTURE);
    for (const hidden of [
      'Desktop action limit',
      'Screenshot limit',
      'Image read budget (MiB)',
      DESKTOP_NOTE,
      CAPTURE_NOTE,
      GATE,
    ]) {
      expect(out).not.toContain(hidden);
    }
  });

  test('desktop on shows its limit, its disclosure and the gate note', () => {
    const out = html({ ...NO_FLEET, desktop: true });
    expect(out).toContain('Desktop action limit');
    expect(out).not.toContain('Screenshot limit');
    expect(out).toContain(DESKTOP_NOTE);
    expect(out).not.toContain('capture the whole selected monitor');
    expect(out).toContain(GATE);
  });

  test('capture on shows the three limits, the capture disclosure and the gate note', () => {
    const out = html({ ...NO_FLEET, capture: true });
    for (const label of ['Desktop action limit', 'Screenshot limit', 'Image read budget (MiB)']) {
      expect(out).toContain(label);
    }
    expect(out).toContain(CAPTURE_NOTE);
    expect(out).not.toContain(DESKTOP_NOTE);
    expect(out).toContain(GATE);
  });

  test('a bad desktop value shows its problem as an alert', () => {
    const out = html({ ...NO_FLEET, capture: true, captureCap: '9999', imageMib: '2000' });
    expect(out).toContain('The screenshot limit is a whole number from 0 to 5000.');
    expect(out).toContain('The image read budget is a whole number of MiB from 0 to 1024.');
    expect(out).toContain('role="alert"');
  });
});
