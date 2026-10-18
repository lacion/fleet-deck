// R13 P6: the limits form and the operations list of a grant (static parts and the save flow).
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { GrantView } from '../gen';
import { station } from '../testing/fleet';
import type { DesktopOpRow, FleetActions, FleetResult } from './fleet';
import {
  GrantDesktop,
  LimitsForm,
  limitCaps,
  limitProblems,
  limitValues,
  opLine,
  raises,
  saveLimits,
} from './GrantDesktop';

const grant: GrantView = {
  grant_id: 'g1',
  station_id: 'station-a',
  session_id: 'coord-1',
  classes: ['read', 'mail', 'drive', 'desktop', 'capture'],
  stations: ['station-a'],
  spawn_cap: 0,
  spawn_used: 0,
  state: 'active',
  revoked_reason: null,
  generation: 1,
  expires_at: 10,
  last_used_at: null,
  desktop_cap: 300,
  desktop_used: 4,
  capture_cap: 20,
  capture_used: 2,
  image_byte_cap: 67_108_864,
  image_byte_used: 0,
};

/** A grant that allows recordings: 5 recordings and 600 seconds, 1 and 30 used. */
const recGrant: GrantView = {
  ...grant,
  recording_cap: 5,
  recording_used: 1,
  recording_second_cap: 600,
  recording_second_used: 30,
};

const actions = (): FleetActions & {
  readonly calls: { grantId: string; caps: object; elevation: string | undefined }[];
} => {
  const calls: { grantId: string; caps: object; elevation: string | undefined }[] = [];
  return {
    calls,
    mail: async () => ({ ok: true, text: '' }),
    task: async () => ({ ok: true, text: '' }),
    tasks: async () => [],
    grants: async () => [],
    revoke: async () => ({ ok: true, text: '' }),
    renew: async () => ({ ok: true, text: '' }),
    caps: async (grantId, caps, elevation) => {
      calls.push({ grantId, caps, elevation });
      return { ok: true, text: 'Limits changed.' };
    },
    desktopOps: async () => [],
  };
};

describe('the limits form', () => {
  test('the values come from the grant, the image budget in MiB', () => {
    expect(limitValues(grant)).toEqual({
      desktopCap: '300',
      captureCap: '20',
      imageMib: '64',
      recordingCap: '0',
      recordingSeconds: '0',
    });
    expect(limitValues(recGrant)).toMatchObject({ recordingCap: '5', recordingSeconds: '600' });
    expect(limitValues({ ...grant, image_byte_cap: 1_572_864 }).imageMib).toBe('1.5');
  });

  test('the form shows the inputs prefilled with Save and Cancel', () => {
    const html = renderToStaticMarkup(
      <LimitsForm
        grant={grant}
        values={limitValues(grant)}
        busy={false}
        onChange={() => {}}
        onSave={() => {}}
        onCancel={() => {}}
      />,
    );
    for (const text of [
      'Desktop action limit',
      'Screenshot limit',
      'Image read budget (MiB)',
      'Save',
      'Cancel',
    ]) {
      expect(html).toContain(text);
    }
    expect(html).toContain('value="300"');
    expect(html).toContain('value="20"');
    expect(html).toContain('value="64"');
  });

  test('the two recording inputs show only for a grant with recording_cap', () => {
    const show = (g: GrantView) =>
      renderToStaticMarkup(
        <LimitsForm
          grant={g}
          values={limitValues(g)}
          busy={false}
          onChange={() => {}}
          onSave={() => {}}
          onCancel={() => {}}
        />,
      );
    const plain = show(grant);
    expect(plain).not.toContain('Recording limit');
    expect(plain).not.toContain('Recording time limit (seconds)');
    const rec = show(recGrant);
    expect(rec).toContain('Recording limit');
    expect(rec).toContain('Recording time limit (seconds)');
    expect(rec).toContain('value="5"');
    expect(rec).toContain('value="600"');
  });

  test('a grant with desktop only shows only the desktop limit', () => {
    const { capture_cap: _c, image_byte_cap: _i, ...rest } = grant;
    const only: GrantView = { ...rest, classes: ['read', 'desktop'] };
    const html = renderToStaticMarkup(
      <LimitsForm
        grant={only}
        values={limitValues(only)}
        busy={false}
        onChange={() => {}}
        onSave={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(html).toContain('Desktop action limit');
    expect(html).not.toContain('Screenshot limit');
    expect(html).not.toContain('Image read budget');
    expect(limitCaps(only, limitValues(only))).toEqual({ desktop_cap: 300 });
  });

  test('the problems use the texts of the spawn form', () => {
    expect(limitProblems(grant, limitValues(grant))).toEqual([]);
    expect(
      limitProblems(grant, {
        ...limitValues(grant),
        desktopCap: '5001',
        captureCap: 'x',
        imageMib: '1025',
      }),
    ).toEqual([
      'The desktop action limit is a whole number from 0 to 5000.',
      'The screenshot limit is a whole number from 0 to 5000.',
      'The image read budget is a whole number of MiB from 0 to 1024.',
    ]);
  });

  test('the recording problems use the texts of the spawn form and only for a recording grant', () => {
    const bad = { ...limitValues(recGrant), recordingCap: '5001', recordingSeconds: 'x' };
    expect(limitProblems(recGrant, bad)).toEqual([
      'The recording limit is a whole number from 0 to 5000.',
      'The recording time limit is a whole number of seconds from 0 to 600000.',
    ]);
    expect(limitProblems(recGrant, { ...bad, recordingSeconds: '600001' })[1]).toBe(
      'The recording time limit is a whole number of seconds from 0 to 600000.',
    );
    expect(limitProblems(grant, bad)).toEqual([]);
  });
});

describe('the save flow', () => {
  const asker = (answer: string | null) => {
    const labels: (readonly string[] | undefined)[] = [];
    return {
      labels,
      ask: async (l?: readonly string[]) => {
        labels.push(l);
        return answer;
      },
    };
  };
  const same = limitValues(grant);

  test('the caps body has the image bytes as MiB x 1,048,576', () => {
    expect(
      limitCaps(grant, { ...same, desktopCap: '1', captureCap: '2', imageMib: '1.5' }),
    ).toEqual({
      desktop_cap: 1,
      capture_cap: 2,
      image_byte_cap: 1_572_864,
    });
  });

  test('the body holds the recording keys only for a recording grant', () => {
    expect(limitCaps(grant, limitValues(grant))).not.toHaveProperty('recording_cap');
    expect(limitCaps(grant, limitValues(grant))).not.toHaveProperty('recording_second_cap');
    expect(limitCaps(recGrant, limitValues(recGrant))).toEqual({
      desktop_cap: 300,
      capture_cap: 20,
      image_byte_cap: 67_108_864,
      recording_cap: 5,
      recording_second_cap: 600,
    });
  });

  test('raises is true only when a value is higher than the grant', () => {
    expect(raises(grant, limitCaps(grant, same))).toBe(false);
    expect(raises(grant, limitCaps(grant, { ...same, desktopCap: '10' }))).toBe(false);
    expect(raises(grant, limitCaps(grant, { ...same, desktopCap: '301' }))).toBe(true);
    expect(raises(grant, limitCaps(grant, { ...same, captureCap: '21' }))).toBe(true);
    expect(raises(grant, limitCaps(grant, { ...same, imageMib: '65' }))).toBe(true);
    const rec = limitValues(recGrant);
    expect(raises(recGrant, limitCaps(recGrant, rec))).toBe(false);
    expect(raises(recGrant, limitCaps(recGrant, { ...rec, recordingCap: '1' }))).toBe(false);
    expect(raises(recGrant, limitCaps(recGrant, { ...rec, recordingCap: '6' }))).toBe(true);
    expect(raises(recGrant, limitCaps(recGrant, { ...rec, recordingSeconds: '601' }))).toBe(true);
  });

  test('a recording decrease does not ask and a recording increase asks Desktop limits', async () => {
    const rec = limitValues(recGrant);
    const a = actions();
    const k = asker('code');
    await saveLimits(recGrant, { ...rec, recordingSeconds: '60' }, a, k.ask);
    expect(k.labels).toEqual([]);
    expect(a.calls[0]).toEqual({
      grantId: 'g1',
      caps: {
        desktop_cap: 300,
        capture_cap: 20,
        image_byte_cap: 67_108_864,
        recording_cap: 5,
        recording_second_cap: 60,
      },
      elevation: undefined,
    });
    const b = actions();
    const j = asker('code-2');
    await saveLimits(recGrant, { ...rec, recordingCap: '6' }, b, j.ask);
    expect(j.labels).toEqual([['Desktop limits']]);
    expect(b.calls[0]?.elevation).toBe('code-2');
    expect(b.calls[0]?.caps).toMatchObject({ recording_cap: 6, recording_second_cap: 600 });
  });

  test('a decrease sends at once with no elevation and does not ask', async () => {
    const a = actions();
    const k = asker('code');
    const r = await saveLimits(grant, { ...same, desktopCap: '10' }, a, k.ask);
    expect(r).toEqual({ ok: true, text: 'Limits changed.' } satisfies FleetResult);
    expect(k.labels).toEqual([]);
    expect(a.calls).toEqual([
      {
        grantId: 'g1',
        caps: { desktop_cap: 10, capture_cap: 20, image_byte_cap: 67_108_864 },
        elevation: undefined,
      },
    ]);
  });

  test('an increase asks once with the Desktop limits label and sends the code', async () => {
    const a = actions();
    const k = asker('code-1');
    await saveLimits(grant, { ...same, captureCap: '25' }, a, k.ask);
    expect(k.labels).toEqual([['Desktop limits']]);
    expect(a.calls.length).toBe(1);
    expect(a.calls[0]?.elevation).toBe('code-1');
  });

  test('a cancelled dialog sends nothing', async () => {
    const a = actions();
    const k = asker(null);
    expect(await saveLimits(grant, { ...same, imageMib: '128' }, a, k.ask)).toBeNull();
    expect(k.labels.length).toBe(1);
    expect(a.calls).toEqual([]);
  });
});

describe('the operations list', () => {
  const stations = [station('station-a'), station('station-b')];
  const row = (extra: Partial<DesktopOpRow>): DesktopOpRow => ({
    operation_id: 'o1',
    tool: 'preview',
    target_station: 'station-b',
    state: 'done',
    code: null,
    created_at: 1,
    updated_at: 2,
    ...extra,
  });

  test('a row is the tool, the station name and the state', () => {
    expect(opLine(row({}), stations)).toBe('preview on Beta: done');
  });

  test('a code shows in brackets', () => {
    expect(opLine(row({ state: 'failed', code: 'station_unreachable' }), stations)).toBe(
      'preview on Beta: failed (station_unreachable)',
    );
  });

  test('prepared, attempted and unknown are marked unresolved; done and failed are not', () => {
    for (const state of ['prepared', 'attempted', 'unknown']) {
      expect(opLine(row({ state }), stations)).toBe(`preview on Beta: ${state} — unresolved`);
    }
    expect(opLine(row({ state: 'failed' }), stations)).not.toContain('unresolved');
    expect(opLine(row({ state: 'unknown', code: 'lost' }), stations)).toBe(
      'preview on Beta: unknown (lost) — unresolved',
    );
  });

  test('a station that is not in the list shows by its id', () => {
    expect(opLine(row({ target_station: 'gone' }), stations)).toBe('preview on gone: done');
  });
});

describe('the grant controls', () => {
  const html = (g: GrantView, canAsk: boolean) =>
    renderToStaticMarkup(
      <GrantDesktop
        grant={g}
        stations={[station('station-a')]}
        actions={actions()}
        ask={canAsk ? async () => 'code' : undefined}
        onChanged={() => {}}
      />,
    );

  test('an active grant of the owner has both buttons', () => {
    const out = html(grant, true);
    expect(out).toContain('Change limits');
    expect(out).toContain('Desktop operations');
  });

  test('a revoked grant has no Change limits button', () => {
    const out = html({ ...grant, state: 'revoked' }, true);
    expect(out).not.toContain('Change limits');
    expect(out).toContain('Desktop operations');
  });

  test('a login without the settings context has no Change limits button', () => {
    expect(html(grant, false)).not.toContain('Change limits');
  });
});
