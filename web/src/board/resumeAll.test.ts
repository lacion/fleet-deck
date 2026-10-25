// G4: "Resume all" counts a pending resume apart from a confirmed one.
import { describe, expect, test } from 'bun:test';
import type { Outcome } from '../session/commands';
import { resumeEach, resumeReport } from './FleetBoard';

const P: Outcome = {
  ok: true,
  text: 'Sent. The station has not confirmed it yet.',
  later: async () => null,
};
const D: Outcome = { ok: true, text: 'Done.' };
const R: Outcome = { ok: false, text: 'Refused: x' };

describe('Resume all (G4)', () => {
  test('resume all counts only a confirmed resume', async () => {
    const count = await resumeEach([
      { resume: async () => D },
      { resume: async () => P },
      { resume: async () => R },
      null,
      {
        resume: async () => {
          throw new Error('boom');
        },
      },
    ]);
    expect(count).toEqual({ confirmed: 1, pending: 1 });
  });

  test('the report names the pending resumes apart', () => {
    expect(resumeReport(2, 0, 3)).toBe('Resumed 2 of 3');
    expect(resumeReport(1, 2, 3)).toBe('Resumed 1 of 3. 2 not confirmed yet');
  });
});
