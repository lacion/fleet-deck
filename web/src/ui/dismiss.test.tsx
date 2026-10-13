import { describe, expect, test } from 'bun:test';
import type { RefObject } from 'react';
import { isOutside } from './dismiss';

/** A fake node: it holds the nodes in `inside`. */
const holder = (...inside: object[]) => ({
  contains: (node: unknown) => inside.includes(node as object),
});
const ref = (
  current: ReturnType<typeof holder> | null,
): RefObject<ReturnType<typeof holder> | null> => ({
  current,
});

describe('isOutside (the test of useDismiss)', () => {
  const child = {};
  const other = {};
  const stray = {};

  test('a target inside the ref is not outside', () => {
    expect(isOutside(child as EventTarget, ref(holder(child)))).toBe(false);
  });

  test('a target outside the ref is outside', () => {
    expect(isOutside(other as EventTarget, ref(holder(child)))).toBe(true);
  });

  test('a target inside the ignored node is not outside', () => {
    expect(isOutside(other as EventTarget, ref(holder(child)), ref(holder(other)))).toBe(false);
    expect(isOutside(stray as EventTarget, ref(holder(child)), ref(holder(other)))).toBe(true);
  });

  test('without a node, every target is outside', () => {
    expect(isOutside(child as EventTarget, ref(null))).toBe(true);
    expect(isOutside(null, ref(holder(child)))).toBe(true);
  });
});
