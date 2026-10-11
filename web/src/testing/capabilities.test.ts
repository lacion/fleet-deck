import { describe, expect, test } from 'bun:test';
import { claudeCaps, codexCaps } from './capabilities';
import { recordedCapabilities } from './timeline';

describe('capability fixtures (P5-3)', () => {
  test('the plain values equal the recorded sessions of the production cores', () => {
    expect(claudeCaps()).toEqual(recordedCapabilities('subagent'));
    expect(claudeCaps()).toEqual(recordedCapabilities('tool_call_approve'));
    expect(codexCaps()).toEqual(recordedCapabilities('codex-tool_call_approve'));
    expect(codexCaps()).toEqual(recordedCapabilities('codex-subagent'));
  });
});
