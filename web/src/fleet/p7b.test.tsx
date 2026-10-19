// R13 P7b §3.1, §14: the grant row texts and the unchanged "Change limits" body.
import { describe, expect, test } from 'bun:test';
import type { GrantView } from '../gen';
import { grantStatus } from './fleet';
import { limitCaps, limitValues, saveLimits } from './GrantDesktop';

const grant: GrantView = {
  grant_id: 'g1',
  station_id: 'station-a',
  session_id: 'coord-1',
  classes: ['read', 'desktop', 'capture', 'input'],
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
  allow_input: true,
  input_scope: 'own_windows',
  input_cap: 100,
  input_used: 7,
  input_character_cap: 20_000,
  input_character_used: 300,
};
const BASE =
  'read, desktop, capture, input, desktop actions 4 of 300, screenshots 2 of 20, image reads 0 of 64 MiB, input actions 7 of 100, typed characters 300 of 20000';

describe('the grant row for the click and the scope', () => {
  test('own windows without clicks adds nothing', () => {
    expect(grantStatus(grant)).toBe(`${BASE}: active`);
    expect(grantStatus({ ...grant, allow_click: false })).toBe(`${BASE}: active`);
  });

  test('clicks allowed', () => {
    expect(grantStatus({ ...grant, allow_click: true })).toBe(`${BASE}, clicks allowed: active`);
  });

  test('input in other application windows', () => {
    expect(grantStatus({ ...grant, input_scope: 'any_window' })).toBe(
      `${BASE}, input in other application windows: active`,
    );
  });

  test('both, after the input part', () => {
    expect(grantStatus({ ...grant, allow_click: true, input_scope: 'any_window' })).toBe(
      `${BASE}, clicks allowed, input in other application windows: active`,
    );
  });
});

describe('Change limits never sends the click or the scope', () => {
  test('the caps body of an any_window grant with clicks has the caps only', async () => {
    const g: GrantView = { ...grant, allow_click: true, input_scope: 'any_window' };
    const caps = limitCaps(g, limitValues(g));
    expect(caps).toEqual({
      desktop_cap: 300,
      capture_cap: 20,
      image_byte_cap: 67_108_864,
      input_cap: 100,
      input_character_cap: 20_000,
    });
    const bodies: object[] = [];
    await saveLimits(
      g,
      { ...limitValues(g), inputCap: '10' },
      {
        caps: async (_id, body) => {
          bodies.push(body);
          return { ok: true, text: '' };
        },
      },
      async () => 'code',
    );
    expect(bodies).toHaveLength(1);
    for (const body of bodies) {
      expect(body).not.toHaveProperty('allow_click');
      expect(body).not.toHaveProperty('input_scope');
      expect(body).not.toHaveProperty('allow_input');
    }
  });
});
