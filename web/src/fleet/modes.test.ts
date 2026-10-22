// The words of the mode ceilings of the fleet access forms (no raw protocol ids for the owner).
import { describe, expect, test } from 'bun:test';
import { CLAUDE_CEILINGS, CODEX_CEILINGS } from '../spawn/FleetControlFields';
import { ceilingLabel, codexModeLabel } from './modes';

describe('mode ceilings in words', () => {
  test('a Codex mode says its approval policy and its sandbox', () => {
    expect(codexModeLabel('on-request/workspace-write')).toBe('Ask on request · workspace write');
    expect(codexModeLabel('never/read-only')).toBe('Never ask · read only');
    expect(codexModeLabel('untrusted/danger-full-access')).toBe('Untrusted · full access');
  });

  test('an unknown Codex id stays as it is', () => {
    expect(codexModeLabel('sometimes/read-only')).toBe('sometimes/read-only');
    expect(codexModeLabel('never/read-only/extra')).toBe('never/read-only/extra');
    expect(codexModeLabel('never')).toBe('never');
  });

  test('a Claude mode has its word from ui/format', () => {
    expect(ceilingLabel('acceptEdits')).toBe('Accept edits');
    expect(ceilingLabel('dontAsk')).toBe("Don't ask");
    expect(ceilingLabel('on-request/read-only')).toBe('Ask on request · read only');
  });

  test('every ceiling of the forms has words', () => {
    for (const mode of [...CLAUDE_CEILINGS, ...CODEX_CEILINGS]) {
      expect(ceilingLabel(mode)).not.toBe(mode);
      expect(ceilingLabel(mode)).not.toMatch(/[a-z]+[A-Z]|\//);
    }
  });
});
