// G4: the pure state functions of the session controller (what the header and the dock show).
import { describe, expect, test } from 'bun:test';
import type { OpenRequest } from '../gen';
import type { Outcome } from './commands';
import {
  dockRefusals,
  EMPTY_SHOWN,
  headerOutcome,
  nextShown,
  onFinal,
  onFirst,
  shownText,
} from './controller';

const P: Outcome = {
  ok: true,
  text: 'Sent. The station has not confirmed it yet.',
  later: async () => null,
};
const R: Outcome = { ok: false, text: 'Refused: no such request' };
const D: Outcome = { ok: true, text: 'Done.' };

const open = (id: string): OpenRequest => ({
  request_id: id,
  node_id: null,
  request_kind: 'tool',
  title: `Run ${id}`,
  details: 'ls',
  input: null,
  options: [{ option_id: 'allow', label: 'Allow', kind: 'allow_once' }],
  response: 'rpc_reply',
  requested_at: 1,
});

describe('nextShown (G4)', () => {
  test('nextShown counts a repeat of the same refusal', () => {
    const a = nextShown(null, 'answer', 'r1', R);
    expect(a.repeats).toBe(1);
    const b = nextShown(a, 'answer', 'r1', R);
    expect(b.repeats).toBe(2);
    expect(shownText(b)).toBe('Refused: no such request (2 times)');
    expect(shownText(a)).toBe('Refused: no such request');
    expect(nextShown(b, 'answer', 'r1', { ok: false, text: 'Refused: other' }).repeats).toBe(1);
    expect(nextShown(b, 'answer', 'r2', R).repeats).toBe(1);
    expect(nextShown(b, 'send', 'r1', R).repeats).toBe(1);
  });
});

describe('the dock and the header (G4)', () => {
  test('an immediate refusal repeated counts in the dock', () => {
    const s1 = onFirst(EMPTY_SHOWN, 'answer', 'r1', R);
    const s2 = onFirst(s1, 'answer', 'r1', R);
    expect(s2.refusals['r1']?.repeats).toBe(2);
    expect(dockRefusals(s2.refusals, [open('r1')])).toEqual({
      r1: 'Refused: no such request (2 times)',
    });
  });

  test('a pending, then refused answer repeated counts', () => {
    const s1 = onFirst(EMPTY_SHOWN, 'answer', 'r1', P);
    const s2 = onFinal(s1, s1.outcome, 'answer', 'r1', R);
    const s3 = onFirst(s2, 'answer', 'r1', P);
    const s4 = onFinal(s3, s3.outcome, 'answer', 'r1', R);
    expect(s4.refusals['r1']?.repeats).toBe(2);
    expect(s4.settled?.repeats).toBe(2);
    expect(s3.refusals['r1']?.repeats).toBe(1);
  });

  test('two pending answers whose receipts come in reverse order both show their refusal', () => {
    const a = onFirst(EMPTY_SHOWN, 'answer', 'rA', P);
    const b = onFirst(a, 'answer', 'rB', P);
    const c = onFinal(b, b.outcome, 'answer', 'rB', R);
    const d = onFinal(c, a.outcome, 'answer', 'rA', R);
    expect(Object.keys(d.refusals).sort()).toEqual(['rA', 'rB']);
    expect(d.refusals['rA']?.repeats).toBe(1);
    expect(d.refusals['rB']?.repeats).toBe(1);
    const requests = [open('rA'), open('rB')];
    expect(Object.keys(dockRefusals(d.refusals, requests)).sort()).toEqual(['rA', 'rB']);
    expect(headerOutcome({ outcome: d.outcome, refusals: d.refusals }, requests)).toBeNull();
  });

  test('a success after a newer outcome stays quiet; a refusal shows', () => {
    const a = onFirst(EMPTY_SHOWN, 'stop', null, P);
    const b = onFirst(a, 'mode', null, R);
    expect(onFinal(b, a.outcome, 'stop', null, D).outcome).toBe(b.outcome);
    expect(onFinal(b, a.outcome, 'stop', null, R).outcome?.action).toBe('stop');
    expect(onFinal(a, a.outcome, 'stop', null, D).outcome?.text).toBe('Done.');
  });

  test('an accepted answer clears its refusal', () => {
    const s1 = onFirst(EMPTY_SHOWN, 'answer', 'r1', R);
    expect(s1.refusals['r1']).toBeDefined();
    const s2 = onFirst(s1, 'answer', 'r1', D);
    expect(s2.refusals['r1']).toBeUndefined();
  });

  test('headerOutcome hides only a refused answer that the dock shows', () => {
    const accepted = onFirst(EMPTY_SHOWN, 'answer', 'r1', D);
    expect(headerOutcome(accepted, [open('r1')])).toBe(accepted.outcome);
    const sendRefused = onFirst(EMPTY_SHOWN, 'send', null, R);
    expect(headerOutcome(sendRefused, [open('r1')])).toBe(sendRefused.outcome);
    const refused = onFirst(EMPTY_SHOWN, 'answer', 'r1', R);
    expect(headerOutcome(refused, [open('r1')])).toBeNull();
    expect(headerOutcome(refused, [])).toBe(refused.outcome);
    expect(headerOutcome({ outcome: refused.outcome, refusals: {} }, [open('r1')])).toBe(
      refused.outcome,
    );
  });
});
