import { describe, expect, test } from 'bun:test';
import { modelFamily, shortModel } from './format';

describe('shortModel', () => {
  test('a Claude id becomes the family and the version', () => {
    expect(shortModel('claude-sonnet-4-5')).toBe('Sonnet 4.5');
    expect(shortModel('claude-opus-4-1-20250805')).toBe('Opus 4.1');
  });

  test('a date or a leading version does not count as the model version', () => {
    expect(shortModel('claude-3-5-sonnet-20241022')).toBe('Sonnet');
  });

  test('a model outside the known families stays as it is', () => {
    expect(shortModel('gpt-5-codex')).toBe('gpt-5-codex');
  });

  test('no model is the default', () => {
    expect(shortModel(null)).toBe('default');
    expect(shortModel('')).toBe('default');
  });
});

describe('modelFamily', () => {
  test('finds the family in any case', () => {
    expect(modelFamily('claude-sonnet-4-5')).toBe('sonnet');
    expect(modelFamily('Claude-OPUS-4')).toBe('opus');
    expect(modelFamily('claude-haiku-4-5')).toBe('haiku');
  });

  test('is null for an unknown family, an empty model and no model', () => {
    expect(modelFamily('gpt-5-codex')).toBeNull();
    expect(modelFamily('')).toBeNull();
    expect(modelFamily(null)).toBeNull();
  });
});
