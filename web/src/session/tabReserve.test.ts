import { expect, test } from 'bun:test';
import { reserveWidth } from './tabReserve';

const rect = (left: number, width: number) => ({ left, width, right: left + width }) as DOMRect;

test('the tab row keeps the room from the left edge of the Show menu to its end', () => {
  // Phone: the menu sits at the end of the row.
  expect(reserveWidth(rect(0, 390), rect(251, 139))).toBe(139);
  // Desktop: the menu ends at the chat panel, left of the properties panel.
  expect(reserveWidth(rect(248, 1192), rect(1070, 80.4))).toBe(370);
});

test('no menu (or a hidden one) keeps no room', () => {
  expect(reserveWidth(rect(0, 390), null)).toBe(0);
  expect(reserveWidth(rect(0, 390), rect(0, 0))).toBe(0);
});
