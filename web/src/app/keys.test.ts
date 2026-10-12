import { describe, expect, test } from 'bun:test';
import { type KeyAction, keyAction, shouldIgnore } from './keys';

const target = (editable: boolean) => ({ closest: () => (editable ? {} : null) });

describe('shouldIgnore', () => {
  test('plain keys outside an input are ours', () => {
    expect(shouldIgnore({ key: 'j', target: target(false) })).toBe(false);
    expect(shouldIgnore({ key: 'j' })).toBe(false);
  });

  test('typing in an input, a textarea or a terminal is not ours', () => {
    expect(shouldIgnore({ key: 'j', target: target(true) })).toBe(true);
  });

  test('IME composition is not ours', () => {
    expect(shouldIgnore({ key: 'j', isComposing: true })).toBe(true);
  });

  test('Ctrl, Meta and Alt chords are not ours', () => {
    expect(shouldIgnore({ key: 'j', ctrlKey: true })).toBe(true);
    expect(shouldIgnore({ key: 'j', metaKey: true })).toBe(true);
    expect(shouldIgnore({ key: 'j', altKey: true })).toBe(true);
  });

  test('Mod+K is never ignored', () => {
    expect(shouldIgnore({ key: 'k', ctrlKey: true, target: target(true) })).toBe(false);
    expect(shouldIgnore({ key: 'K', metaKey: true, target: target(true) })).toBe(false);
  });
});

describe('keyAction', () => {
  test('maps the global keys', () => {
    const table: Record<string, KeyAction> = {
      j: 'next',
      k: 'prev',
      '/': 'focus',
      n: 'new',
      f: 'fleet',
      t: 'terminals',
      '?': 'help',
      Escape: 'escape',
    };
    for (const [key, action] of Object.entries(table)) expect(keyAction({ key })).toBe(action);
  });

  test('Mod+K opens the palette, also from an input', () => {
    expect(keyAction({ key: 'k', ctrlKey: true, target: target(true) })).toBe('palette');
    expect(keyAction({ key: 'k', metaKey: true })).toBe('palette');
  });

  test('unknown keys and ignored targets give nothing', () => {
    expect(keyAction({ key: 'x' })).toBeNull();
    expect(keyAction({ key: 'j', target: target(true) })).toBeNull();
    expect(keyAction({ key: 'j', isComposing: true })).toBeNull();
  });
});
