import { describe, expect, test } from 'bun:test';
import { errorSentence, isRiskyMode, modelFamily, plainLine, shortModel, sizeText } from './format';

describe('shortModel', () => {
  test('a Claude id becomes the family and the version', () => {
    expect(shortModel('claude-sonnet-4-5')).toBe('Sonnet 4.5');
    expect(shortModel('claude-opus-4-1-20250805')).toBe('Opus 4.1');
  });

  test('a context suffix keeps the minor version and shows after it', () => {
    expect(shortModel('claude-opus-5-5[1m]')).toBe('Opus 5.5 1M');
    expect(shortModel('claude-opus-5-5')).toBe('Opus 5.5');
    expect(shortModel('opus[1m]')).toBe('Opus 1M');
  });

  test('a date or a leading version does not count as the model version', () => {
    expect(shortModel('claude-3-5-sonnet-20241022')).toBe('Sonnet');
  });

  test('a GPT id becomes GPT, the version and the variant', () => {
    expect(shortModel('gpt-6.1-codex')).toBe('GPT-6.1 Codex');
    expect(shortModel('gpt-5-codex')).toBe('GPT-5 Codex');
    expect(shortModel('gpt-6')).toBe('GPT-6');
  });

  test('a model outside the known families stays as it is', () => {
    expect(shortModel('o3-mini')).toBe('o3-mini');
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

describe('plainLine (DL-171)', () => {
  test('strips heading marks, emphasis, code ticks and links', () => {
    expect(plainLine('## Plan')).toBe('Plan');
    expect(plainLine('**Finished:** Nothing yet.')).toBe('Finished: Nothing yet.');
    expect(plainLine('Current model: `Opus 5.5 (1M context)` (effort: high)')).toBe(
      'Current model: Opus 5.5 (1M context) (effort: high)',
    );
    expect(plainLine('See [the docs](https://x.test/a) and *this* and _that_.')).toBe(
      'See the docs and this and that.',
    );
    expect(plainLine('> quoted ~~gone~~')).toBe('quoted gone');
  });

  test('keeps a snake_case word and a lone star, and collapses spaces', () => {
    expect(plainLine('a snake_case_name and 2 * 3')).toBe('a snake_case_name and 2 * 3');
    expect(plainLine('  two   spaces  ')).toBe('two spaces');
  });
});

describe('errorSentence (DL-171)', () => {
  test('a code: detail text becomes the detail in a sentence', () => {
    expect(errorSentence('rate_limit: Try later')).toBe('Try later.');
    expect(errorSentence('not_dispatched: stop was not sent: already_stopping')).toBe(
      'Stop was not sent: the session was already stopping.',
    );
  });

  test('a text without a code is not changed', () => {
    expect(errorSentence('The station did not answer.')).toBe('The station did not answer.');
    expect(errorSentence('Failed: x')).toBe('Failed: x');
  });
});

describe('isRiskyMode (DL-171)', () => {
  test('only the modes that skip the approvals', () => {
    for (const m of ['dontAsk', 'bypassPermissions', 'danger-full-access']) {
      expect(isRiskyMode(m)).toBe(true);
    }
    for (const m of ['plan', 'default', 'acceptEdits', null, undefined]) {
      expect(isRiskyMode(m)).toBe(false);
    }
  });
});

test('sizeText uses decimal units', () => {
  expect(sizeText(512)).toBe('512 B');
  expect(sizeText(3200)).toBe('3.2 KB');
  expect(sizeText(12_345)).toBe('12.3 KB');
  expect(sizeText(4_500_000)).toBe('4.5 MB');
  expect(sizeText(1_200_000_000)).toBe('1.2 GB');
});
