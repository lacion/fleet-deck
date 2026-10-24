import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FleetControlFields } from './FleetControlFields';
import {
  type FleetControl,
  answerCapProblem,
  captureCapProblem,
  desktopCapProblem,
  fleetBlocked,
  fleetElevation,
  fleetOn,
  idleLimitProblem,
  grantFields,
  fleetProblem,
  imageBudgetProblem,
  inputCapProblem,
  inputCharsProblem,
  inputOn,
  NO_FLEET,
  recordingCapProblem,
  recordingSecondsProblem,
  showFleetFields,
  spawnCapProblem,
} from './fleetControl';
import { spawnActions } from './spawn';

const PROBLEM = 'The answer limit is a whole number.';
const spec = {
  provider_instance: 'claude',
  cwd: '/home/me/app',
  mode: null,
  model: null,
  title: null,
};

describe('grantFields (ADR-008 §3)', () => {
  test('all switches off give exactly the lineage classes', () => {
    expect(grantFields(NO_FLEET)).toEqual({
      classes: ['read', 'mail', 'drive'],
      idle_ttl_ms: 1_800_000,
    });
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
      idle_ttl_ms: 1_800_000,
      scope: 'fleet',
    });
    expect(grantFields({ ...NO_FLEET, answer: true, answerCap: '12' })).toEqual({
      classes: ['read', 'mail', 'drive', 'answer'],
      idle_ttl_ms: 1_800_000,
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
      idle_ttl_ms: 1_800_000,
      scope: 'fleet',
      answer_cap: 0,
      max_modes: { claude: 'acceptEdits', codex: 'on-request/workspace-write' },
    });
    expect(fleetOn({ ...NO_FLEET, manage: true })).toBe(true);
  });
});

describe('the spawn switch (MCP P0 §2.5)', () => {
  test('the defaults are off, 3, acceptEdits and on-request/workspace-write', () => {
    expect(NO_FLEET.spawn).toBe(false);
    expect(NO_FLEET.spawnCap).toBe('3');
    expect(NO_FLEET.claudeCeiling).toBe('acceptEdits');
    expect(NO_FLEET.codexCeiling).toBe('on-request/workspace-write');
    expect(fleetOn({ ...NO_FLEET, spawn: true })).toBe(false);
  });
  test('spawn off sends neither spawn_cap nor max_modes', () => {
    expect(grantFields(NO_FLEET)).toEqual({
      classes: ['read', 'mail', 'drive'],
      idle_ttl_ms: 1_800_000,
    });
  });
  test('spawn on adds the class after drive, the cap and both ceilings', () => {
    expect(grantFields({ ...NO_FLEET, spawn: true, spawnCap: ' 7 ', inspect: true })).toEqual({
      classes: ['read', 'mail', 'drive', 'spawn', 'inspect'],
      idle_ttl_ms: 1_800_000,
      spawn_cap: 7,
      max_modes: { claude: 'acceptEdits', codex: 'on-request/workspace-write' },
    });
  });
  test('manage alone sends max_modes and no spawn_cap', () => {
    expect(
      grantFields({
        ...NO_FLEET,
        manage: true,
        claudeCeiling: 'plan',
        codexCeiling: 'never/read-only',
      }),
    ).toEqual({
      classes: ['read', 'mail', 'drive', 'manage'],
      idle_ttl_ms: 1_800_000,
      max_modes: { claude: 'plan', codex: 'never/read-only' },
    });
  });
  test('spawnCapProblem', () => {
    const PROBLEM_TEXT = 'The session limit is a whole number.';
    const on = (spawnCap: string) => spawnCapProblem({ ...NO_FLEET, spawn: true, spawnCap });
    for (const bad of ['', '2.5', '-1']) expect(on(bad)).toBe(PROBLEM_TEXT);
    for (const ok of ['0', '50', '51', '4000000']) expect(on(ok)).toBeNull();
    expect(spawnCapProblem({ ...NO_FLEET, spawn: false, spawnCap: '' })).toBeNull();
    expect(fleetProblem({ ...NO_FLEET, spawn: true, spawnCap: '' })).toBe(PROBLEM_TEXT);
  });
  test('fleetBlocked', () => {
    const blank = { ...NO_FLEET, spawn: true, spawnCap: '' };
    expect(fleetBlocked(true, true, blank)).toBe(true);
    expect(fleetBlocked(true, true, { ...blank, spawnCap: '3' })).toBe(false);
    expect(fleetBlocked(false, true, blank)).toBe(false);
    expect(fleetBlocked(true, false, blank)).toBe(false);
  });
  test('the fields show the group, the limit and the ceilings only when needed', () => {
    const render = (fleet: FleetControl) =>
      renderToStaticMarkup(<FleetControlFields fleet={fleet} onChange={() => {}} />);
    const off = render(NO_FLEET);
    expect(off).toContain('Sessions it may start');
    expect(off).toContain('Allow starting sessions');
    expect(off).not.toContain('Session limit');
    expect(off).not.toContain('Highest Claude mode');
    const on = render({ ...NO_FLEET, spawn: true });
    expect(on).toContain('Session limit');
    expect(on).toContain('Highest Claude mode');
    expect(on).toContain('Highest Codex mode');
    const manage = render({ ...NO_FLEET, manage: true });
    expect(manage).not.toContain('Session limit');
    expect(manage).toContain('Highest Claude mode');
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
      idle_ttl_ms: 1_800_000,
      desktop_cap: 12,
    });
  });
  test('capture adds its class and all three limits, the image bytes in MiB x 1,048,576', () => {
    expect(grantFields({ ...NO_FLEET, capture: true })).toEqual({
      classes: ['read', 'mail', 'drive', 'capture'],
      idle_ttl_ms: 1_800_000,
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
      idle_ttl_ms: 1_800_000,
      desktop_cap: 7,
      capture_cap: 20,
      image_byte_cap: 1_048_576,
    });
  });
});

describe('the recording opt-in (R13 P5b-2)', () => {
  const RC = 'The recording limit is a whole number from 0 to 5000.';
  const RS = 'The recording time limit is a whole number of seconds from 0 to 600000.';
  const on = { ...NO_FLEET, capture: true, recording: true };
  test('the defaults are off, 5 and 600', () => {
    expect(NO_FLEET.recording).toBe(false);
    expect(NO_FLEET.recordingCap).toBe('5');
    expect(NO_FLEET.recordingSeconds).toBe('600');
  });
  test('grantFields adds the opt-in only with capture and recording on', () => {
    expect(grantFields({ ...NO_FLEET, capture: true })).not.toHaveProperty('allow_recording');
    expect(grantFields({ ...NO_FLEET, recording: true })).toEqual({
      classes: ['read', 'mail', 'drive'],
      idle_ttl_ms: 1_800_000,
    });
    expect(grantFields({ ...on, recordingCap: '7', recordingSeconds: '90' })).toEqual({
      classes: ['read', 'mail', 'drive', 'capture'],
      idle_ttl_ms: 1_800_000,
      desktop_cap: 300,
      capture_cap: 20,
      image_byte_cap: 67_108_864,
      allow_recording: true,
      recording_cap: 7,
      recording_second_cap: 90,
    });
    expect(grantFields(on)).toMatchObject({ recording_cap: 5, recording_second_cap: 600 });
  });
  test('the problems are checked with capture and recording on, ranges 0-5000 and 0-600000', () => {
    expect(recordingCapProblem({ ...NO_FLEET, recordingCap: 'x' })).toBeNull();
    expect(recordingCapProblem({ ...on, recording: false, recordingCap: 'x' })).toBeNull();
    expect(recordingCapProblem({ ...NO_FLEET, recording: true, recordingCap: 'x' })).toBeNull();
    expect(recordingCapProblem(on)).toBeNull();
    expect(recordingCapProblem({ ...on, recordingCap: '0' })).toBeNull();
    expect(recordingCapProblem({ ...on, recordingCap: '5000' })).toBeNull();
    for (const bad of ['5001', '-1', '1.5', '', 'x']) {
      expect(recordingCapProblem({ ...on, recordingCap: bad })).toBe(RC);
    }
    expect(recordingSecondsProblem({ ...NO_FLEET, recordingSeconds: 'x' })).toBeNull();
    expect(recordingSecondsProblem({ ...on, recordingSeconds: '600000' })).toBeNull();
    expect(recordingSecondsProblem({ ...on, recordingSeconds: '0' })).toBeNull();
    for (const bad of ['600001', '-1', '1.5', '', 'x']) {
      expect(recordingSecondsProblem({ ...on, recordingSeconds: bad })).toBe(RS);
    }
    expect(fleetProblem({ ...on, recordingCap: 'x' })).toBe(RC);
    expect(fleetProblem({ ...on, recordingSeconds: 'x' })).toBe(RS);
    expect(fleetProblem(on)).toBeNull();
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
  test('0, 500, 5000 and 5001 are fine: the deck checks the upper bound', () => {
    for (const ok of ['500', '0', '5000', '5001']) expect(on(ok)).toBeNull();
  });
  test('-1, 1.5, empty and text give the problem', () => {
    for (const bad of ['-1', '1.5', '', 'abc']) expect(on(bad)).toBe(PROBLEM);
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

  test('a spawn-only grant asks once with the Starting sessions label', async () => {
    const a = asker('code-3');
    const fleet = { ...NO_FLEET, spawn: true };
    expect(await fleetElevation({ ...gate, fleet }, a.ask)).toEqual({
      ok: true,
      elevation: 'code-3',
    });
    expect(a.calls).toEqual([['Starting sessions']]);
  });

  test('spawn with a switch on or with targets keeps the Fleet control label', async () => {
    const a = asker('code-4');
    await fleetElevation({ ...gate, fleet: { ...NO_FLEET, spawn: true, inspect: true } }, a.ask);
    await fleetElevation(
      { ...gate, fleet: { ...NO_FLEET, spawn: true }, targets: ['station-b'] },
      a.ask,
    );
    expect(a.calls).toEqual([['Fleet control'], ['Fleet control']]);
  });

  test('a coordinator start with all switches and spawn off asks nothing', async () => {
    const a = asker('code-5');
    expect(await fleetElevation({ ...gate, fleet: NO_FLEET }, a.ask)).toEqual({
      ok: true,
      elevation: undefined,
    });
    expect(a.calls).toEqual([]);
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

  test('the answer limit shows with answer on, with the error for text', () => {
    const ok = html({ ...NO_FLEET, answer: true });
    expect(ok).toContain('Answer limit');
    expect(ok).toContain('confirm it is you');
    expect(ok).not.toContain(PROBLEM);
    const bad = html({ ...NO_FLEET, answer: true, answerCap: 'abc' });
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

  const REC = 'Allow monitor recordings';
  // The markup escapes the apostrophe.
  const REC_NOTE =
    'This agent can record the whole selected monitor for up to the station&#x27;s longest recording (120 seconds by default) at a time, including other applications, notifications and private information visible there. Recordings are stored as video artifacts. The agent gets only a reference to each recording, never the video.';

  test('the recording switch shows only while capture is on, with nothing else while off', () => {
    for (const fleet of [NO_FLEET, { ...NO_FLEET, desktop: true }]) {
      const out = html(fleet);
      for (const hidden of [REC, 'Recording limit', 'Recording time limit (seconds)', REC_NOTE]) {
        expect(out).not.toContain(hidden);
      }
    }
    const out = html({ ...NO_FLEET, capture: true });
    expect(out).toContain(REC);
    for (const hidden of ['Recording limit', 'Recording time limit (seconds)', REC_NOTE]) {
      expect(out).not.toContain(hidden);
    }
  });

  test('recording on shows its two limits and the disclosure', () => {
    const out = html({ ...NO_FLEET, capture: true, recording: true });
    for (const text of [REC, 'Recording limit', 'Recording time limit (seconds)', REC_NOTE]) {
      expect(out).toContain(text);
    }
  });

  test('a bad recording value shows its problem as an alert', () => {
    const out = html({
      ...NO_FLEET,
      capture: true,
      recording: true,
      recordingCap: '5001',
      recordingSeconds: '600001',
    });
    expect(out).toContain('The recording limit is a whole number from 0 to 5000.');
    expect(out).toContain(
      'The recording time limit is a whole number of seconds from 0 to 600000.',
    );
    expect(out).toContain('role="alert"');
  });

  test('turning capture off turns recording off, and recording changes only itself', () => {
    const calls: FleetControl[] = [];
    const tree = FleetControlFields({
      fleet: { ...NO_FLEET, capture: true, recording: true },
      onChange: (next) => calls.push(next),
    });
    const boxes: { label: string; onChange: (e: { target: { checked: boolean } }) => void }[] = [];
    const walk = (node: unknown): void => {
      if (Array.isArray(node)) {
        node.forEach(walk);
        return;
      }
      if (typeof node !== 'object' || node === null) return;
      const el = node as { type?: unknown; props?: Record<string, unknown> };
      const kids = el.props?.['children'];
      if (el.type === 'label' && Array.isArray(kids)) {
        const input = kids.find(
          (k): k is { props: { onChange: never } } =>
            typeof k === 'object' && k !== null && (k as { type?: unknown }).type === 'input',
        );
        const text = kids.filter((k) => typeof k === 'string').join('');
        if (input && text) boxes.push({ label: text, onChange: input.props.onChange });
      }
      walk(kids);
    };
    walk(tree);
    const capture = boxes.find((b) => b.label.startsWith('Screenshots:'));
    const recording = boxes.find((b) => b.label === REC);
    capture?.onChange({ target: { checked: false } });
    recording?.onChange({ target: { checked: false } });
    expect(calls[0]).toMatchObject({ capture: false, recording: false });
    expect(calls[1]).toMatchObject({ capture: true, recording: false });
  });
});

describe('the input opt-in (R13 P7 §3.4, §9.1)', () => {
  const INPUT = "Allow keyboard input and pointer movement in this agent's previews";
  const DISCLOSURE =
    'This agent can type, press keys and move the pointer in preview windows opened for it on the grant stations. Keyboard input can activate buttons and submit forms. Focus can change after Fleet Deck checks it, so input may reach another window, including an owner board window. Board detection is incomplete. Agents run as your OS user. Pointer clicks and unrestricted window input are unavailable. This feature is experimental until its live smoke passes.';
  const SCOPE = "Scope: this caller's preview windows";
  const SIGN_OUT =
    'While an agent types on a station, every board signed in on that station is signed out. Keep this board on another device.';
  const on = { ...NO_FLEET, desktop: true, capture: true, input: true };
  const html = (fleet: FleetControl) =>
    renderToStaticMarkup(<FleetControlFields fleet={fleet} onChange={() => {}} />).replaceAll(
      '&#x27;',
      "'",
    );

  test('the defaults are off, 100 actions and 20000 characters', () => {
    expect(NO_FLEET.input).toBe(false);
    expect(NO_FLEET.inputCap).toBe('100');
    expect(NO_FLEET.inputChars).toBe('20000');
  });

  test('the switch shows only while both desktop and capture are on', () => {
    expect(html(NO_FLEET)).not.toContain(INPUT);
    expect(html({ ...NO_FLEET, desktop: true })).not.toContain(INPUT);
    expect(html({ ...NO_FLEET, capture: true })).not.toContain(INPUT);
    expect(html({ ...NO_FLEET, desktop: true, capture: true })).toContain(INPUT);
  });

  test('input off shows no limit, scope, disclosure or sign-out note', () => {
    const out = html({ ...NO_FLEET, desktop: true, capture: true });
    for (const hidden of [
      'Input action limit',
      'Typed character limit',
      SCOPE,
      DISCLOSURE,
      SIGN_OUT,
    ]) {
      expect(out).not.toContain(hidden);
    }
  });

  test('input on shows the scope, the two limits, the red disclosure and the sign-out note', () => {
    const out = html(on);
    expect(out).toContain(SCOPE);
    expect(out).toContain('Input action limit');
    expect(out).toContain('Typed character limit');
    expect(out).toContain('value="100"');
    expect(out).toContain('value="20000"');
    expect(out).toContain(`<p class="fd-note fd-fleet-failed">${DISCLOSURE}</p>`);
    expect(out).toContain(`<p class="fd-note">${SIGN_OUT}</p>`);
  });

  test('a bad value shows its problem as an alert', () => {
    const out = html({ ...on, inputCap: '5001', inputChars: '1000001' });
    expect(out).toContain(
      '<p class="fd-note" role="alert">The input action limit is a whole number from 0 to 5000.</p>',
    );
    expect(out).toContain(
      '<p class="fd-note" role="alert">The typed character limit is a whole number from 0 to 1000000.</p>',
    );
  });

  test('the problems check the ranges, and only while the opt-in is on', () => {
    expect(inputCapProblem(on)).toBeNull();
    expect(inputCharsProblem(on)).toBeNull();
    for (const ok of ['0', '5000']) expect(inputCapProblem({ ...on, inputCap: ok })).toBeNull();
    for (const bad of ['5001', '-1', '1.5', '', 'x']) {
      expect(inputCapProblem({ ...on, inputCap: bad })).toBe(
        'The input action limit is a whole number from 0 to 5000.',
      );
    }
    for (const ok of ['0', '1000000']) {
      expect(inputCharsProblem({ ...on, inputChars: ok })).toBeNull();
    }
    for (const bad of ['1000001', '-1', '1.5', '', 'x']) {
      expect(inputCharsProblem({ ...on, inputChars: bad })).toBe(
        'The typed character limit is a whole number from 0 to 1000000.',
      );
    }
    const off = { ...NO_FLEET, inputCap: 'x', inputChars: 'x' };
    expect(inputCapProblem(off)).toBeNull();
    expect(inputCharsProblem(off)).toBeNull();
    expect(fleetProblem({ ...on, inputCap: 'x' })).toBe(
      'The input action limit is a whole number from 0 to 5000.',
    );
    expect(fleetProblem({ ...on, inputChars: 'x' })).toBe(
      'The typed character limit is a whole number from 0 to 1000000.',
    );
    expect(fleetProblem(on)).toBeNull();
  });

  test('grantFields adds the input fields only with the opt-in, desktop and capture', () => {
    const plain = grantFields({ ...NO_FLEET, desktop: true, capture: true });
    expect(plain).not.toHaveProperty('allow_input');
    expect(plain).not.toHaveProperty('input_scope');
    expect(plain).not.toHaveProperty('input_cap');
    expect(plain).not.toHaveProperty('input_character_cap');
    // The switch alone, without desktop and capture, is not an opt-in.
    expect(inputOn({ ...NO_FLEET, input: true, desktop: true })).toBe(false);
    expect(grantFields({ ...NO_FLEET, input: true, desktop: true })).not.toHaveProperty(
      'allow_input',
    );
    expect(grantFields(on)).toMatchObject({
      allow_input: true,
      input_scope: 'own_windows',
      input_cap: 100,
      input_character_cap: 20_000,
    });
    expect(grantFields({ ...on, inputCap: '0', inputChars: '5' })).toMatchObject({
      input_cap: 0,
      input_character_cap: 5,
    });
  });

  test('the elevation label stays Fleet control', async () => {
    const labels: (readonly string[] | undefined)[] = [];
    const result = await fleetElevation(
      { coordinator: true, importing: false, fleet: on },
      async (l) => {
        labels.push(l);
        return 'code';
      },
    );
    expect(result).toEqual({ ok: true, elevation: 'code' });
    expect(labels).toEqual([['Fleet control']]);
  });

  test('turning desktop or capture off turns input off, and input changes only itself', () => {
    const calls: FleetControl[] = [];
    const tree = FleetControlFields({ fleet: on, onChange: (next) => calls.push(next) });
    const boxes: { label: string; onChange: (e: { target: { checked: boolean } }) => void }[] = [];
    const walk = (node: unknown): void => {
      if (Array.isArray(node)) {
        node.forEach(walk);
        return;
      }
      if (typeof node !== 'object' || node === null) return;
      const el = node as { type?: unknown; props?: Record<string, unknown> };
      const kids = el.props?.['children'];
      if (el.type === 'label' && Array.isArray(kids)) {
        const input = kids.find(
          (k): k is { props: { onChange: never } } =>
            typeof k === 'object' && k !== null && (k as { type?: unknown }).type === 'input',
        );
        const text = kids.filter((k) => typeof k === 'string').join('');
        if (input && text) boxes.push({ label: text, onChange: input.props.onChange });
      }
      walk(kids);
    };
    walk(tree);
    boxes.find((b) => b.label.startsWith('Desktop:'))?.onChange({ target: { checked: false } });
    boxes.find((b) => b.label.startsWith('Screenshots:'))?.onChange({ target: { checked: false } });
    boxes.find((b) => b.label === INPUT)?.onChange({ target: { checked: false } });
    expect(calls[0]).toMatchObject({ desktop: false, input: false });
    expect(calls[1]).toMatchObject({ capture: false, recording: false, input: false });
    expect(calls[2]).toMatchObject({ desktop: true, capture: true, input: false });
  });
});

describe('"No limit" and the idle limit (B27 §3.7)', () => {
  const NONE = 4_294_967_295;
  const IDLE = 'The idle limit is a whole number of minutes from 1 to 10080.';
  test('the idle limit is 30 minutes by default and always sent', () => {
    expect(NO_FLEET.idleMinutes).toBe('30');
    expect(grantFields(NO_FLEET).idle_ttl_ms).toBe(1_800_000);
    expect(grantFields({ ...NO_FLEET, idleMinutes: ' 45 ' }).idle_ttl_ms).toBe(2_700_000);
  });
  test('"No limit" sends 4294967295 for the idle, session and answer limits', () => {
    expect(grantFields({ ...NO_FLEET, idleNone: true }).idle_ttl_ms).toBe(NONE);
    const f = { ...NO_FLEET, spawn: true, spawnNone: true, answer: true, answerNone: true };
    expect(grantFields(f).spawn_cap).toBe(NONE);
    expect(grantFields(f).answer_cap).toBe(NONE);
    expect(grantFields({ ...f, spawnNone: false, spawnCap: '4' }).spawn_cap).toBe(4);
  });
  test('"No limit" turns the text problem off', () => {
    const f = { ...NO_FLEET, spawn: true, spawnCap: '', answer: true, answerCap: '' };
    expect(fleetProblem(f)).toBe('The session limit is a whole number.');
    expect(fleetProblem({ ...f, spawnNone: true })).toBe(PROBLEM);
    expect(fleetProblem({ ...f, spawnNone: true, answerNone: true })).toBeNull();
  });
  test('the idle problem text, with no problem while "No limit" is on', () => {
    for (const bad of ['', '0', '10081', '1.5', 'x']) {
      expect(idleLimitProblem({ ...NO_FLEET, idleMinutes: bad })).toBe(IDLE);
    }
    for (const ok of ['1', '30', '10080']) {
      expect(idleLimitProblem({ ...NO_FLEET, idleMinutes: ok })).toBeNull();
    }
    expect(idleLimitProblem({ ...NO_FLEET, idleMinutes: '', idleNone: true })).toBeNull();
    expect(fleetProblem({ ...NO_FLEET, idleMinutes: '0' })).toBe(IDLE);
  });
  test('the fields show the idle input and the "No limit" boxes, with the input off while checked', () => {
    const render = (fleet: FleetControl, live = false) =>
      renderToStaticMarkup(<FleetControlFields fleet={fleet} onChange={() => {}} live={live} />);
    const html = render({ ...NO_FLEET, spawn: true, answer: true });
    expect(html).toContain('Idle limit (minutes)');
    expect(html).toContain('aria-label="Idle limit (minutes): no limit"');
    expect(html).toContain('aria-label="Session limit: no limit"');
    expect(html).toContain('aria-label="Answer limit: no limit"');
    expect(html).not.toContain('disabled=""');
    expect(render({ ...NO_FLEET, idleNone: true })).toContain('disabled=""');
    expect(render(NO_FLEET, true)).not.toContain('Idle limit');
  });
});
