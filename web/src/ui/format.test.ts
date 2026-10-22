import { describe, expect, test } from 'bun:test';
import {
  addedText,
  compactCount,
  contextLabel,
  countText,
  errorSentence,
  homeShort,
  homeShortIn,
  isRiskyMode,
  modelFamily,
  pathParts,
  plainLine,
  removedText,
  shortModel,
  sizeText,
  usdText,
} from './format';

describe('shortModel', () => {
  test('a Claude id becomes the family and the version', () => {
    expect(shortModel('claude-sonnet-4-5')).toBe('Sonnet 4.5');
    expect(shortModel('claude-opus-4-1-20250805')).toBe('Opus 4.1');
  });

  test('a context suffix is not part of the name and keeps the minor version', () => {
    expect(shortModel('claude-opus-5-5[1m]')).toBe('Opus 5.5');
    expect(shortModel('claude-opus-5-5')).toBe('Opus 5.5');
    expect(shortModel('opus[1m]')).toBe('Opus');
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

describe('contextLabel', () => {
  test('the context suffix of a model id', () => {
    expect(contextLabel('claude-opus-5-5[1m]')).toBe('1M context');
    expect(contextLabel('opus[200k]')).toBe('200K context');
  });

  test('is null for an id without a suffix and for no model', () => {
    expect(contextLabel('claude-opus-5-5')).toBeNull();
    expect(contextLabel('opus[]')).toBeNull();
    expect(contextLabel(null)).toBeNull();
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

describe('numbers (DESIGN-SYSTEM §5)', () => {
  test('countText groups the thousands', () => {
    expect(countText(0)).toBe('0');
    expect(countText(940)).toBe('940');
    expect(countText(1286)).toBe('1,286');
    expect(countText(36_952)).toBe('36,952');
    expect(countText(1_234_567)).toBe('1,234,567');
  });

  test('compactCount keeps two significant digits and no trailing zero', () => {
    expect(compactCount(522)).toBe('522');
    expect(compactCount(940)).toBe('940');
    expect(compactCount(1000)).toBe('1K');
    expect(compactCount(3100)).toBe('3.1K');
    expect(compactCount(9400)).toBe('9.4K');
    expect(compactCount(52_300)).toBe('52K');
    expect(compactCount(236_400)).toBe('236K');
    expect(compactCount(999_999)).toBe('1M');
    expect(compactCount(1_000_000)).toBe('1M');
    expect(compactCount(1_250_000)).toBe('1.3M');
    expect(compactCount(12_345_678)).toBe('12M');
  });

  test('usdText groups the thousands and keeps the cents', () => {
    expect(usdText(0)).toBe('$0.00');
    expect(usdText(1.2)).toBe('$1.20');
    expect(usdText(14_876.41)).toBe('$14,876.41');
    expect(usdText(51_735.857)).toBe('$51,735.86');
  });

  test('added and removed lines carry a sign and separators', () => {
    expect(addedText(1286)).toBe('+1,286');
    expect(removedText(400)).toBe('\u2212400');
    expect(removedText(36_952)).toBe('\u221236,952');
  });
});

describe('paths (DESIGN-SYSTEM §5)', () => {
  test('homeShort starts a path under a home folder at ~', () => {
    expect(homeShort('/home/me/projects/x')).toBe('~/projects/x');
    expect(homeShort('/Users/me/x')).toBe('~/x');
    expect(homeShort('/home/me')).toBe('~');
    expect(homeShort('/home/me2x/a')).toBe('~/a');
    expect(homeShort('/srv/data')).toBe('/srv/data');
    expect(homeShort('/home')).toBe('/home');
  });

  test('homeShortIn shortens every home folder that starts a word of a text', () => {
    expect(homeShortIn('cat /home/me/a.txt')).toBe('cat ~/a.txt');
    expect(homeShortIn('/home/me/x && ls /Users/you')).toBe('~/x && ls ~');
    expect(homeShortIn("--dir=/home/me/x '/home/me/y' (/home/me/z)")).toBe("--dir=~/x '~/y' (~/z)");
    expect(homeShortIn('a/home/me/b /home /srv/home/me')).toBe('a/home/me/b /home /srv/home/me');
    expect(homeShortIn('cargo test')).toBe('cargo test');
  });

  test('pathParts gives the name and the parent from its root', () => {
    expect(pathParts('/home/me/fleet-deck-worktrees/v2')).toEqual({
      parent: '~/fleet-deck-worktrees/',
      base: 'v2',
    });
    expect(pathParts('/home/me/fleet/')).toEqual({ parent: '~/', base: 'fleet' });
    expect(pathParts('/home/me')).toEqual({ parent: '', base: '~' });
    expect(pathParts('/srv/data')).toEqual({ parent: '/srv/', base: 'data' });
    expect(pathParts('/srv')).toEqual({ parent: '/', base: 'srv' });
    expect(pathParts('/')).toEqual({ parent: '', base: '/' });
  });
});
