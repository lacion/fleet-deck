import { describe, expect, test } from 'bun:test';
import type { BoardServerMessage, ChatMessage } from '../gen';
import { addChat, CHAT_KEEP, emptyState, reduceMessage, sessionKey } from './state';

const key = { station_id: 'station', session_id: 'session' };
const id = sessionKey(key);
const chat = (seq: number, text = `m${seq}`, author = 'member:Ana'): ChatMessage => ({
  seq,
  author,
  text,
  at: seq,
});
const apply = (messages: BoardServerMessage[], from = emptyState()) =>
  messages.reduce(reduceMessage, from);

describe('people reducers (R11, DL-166)', () => {
  test('presence replaces the people of the session', () => {
    const one = apply([{ type: 'presence', ...key, who: ['owner'] }]);
    expect(one.presence[id]).toEqual(['owner']);
    const two = apply([{ type: 'presence', ...key, who: ['owner', 'member:Ana'] }], one);
    expect(two.presence[id]).toEqual(['owner', 'member:Ana']);
    const other = apply(
      [{ type: 'presence', station_id: 'station', session_id: 'other', who: ['owner'] }],
      two,
    );
    expect(other.presence[id]).toEqual(['owner', 'member:Ana']);
  });

  test('a chat message is added once and the chat stays in order', () => {
    const state = apply([
      { type: 'chat', ...key, message: chat(2) },
      { type: 'chat', ...key, message: chat(1) },
      { type: 'chat', ...key, message: chat(2, 'again') },
    ]);
    expect(state.chat[id]?.map((m) => m.seq)).toEqual([1, 2]);
    expect(state.chat[id]?.[1]?.text).toBe('m2');
  });

  test('the board keeps the newest messages only', () => {
    let held: readonly ChatMessage[] = [];
    for (let seq = 1; seq <= CHAT_KEEP + 3; seq += 1) held = addChat(held, chat(seq));
    expect(held).toHaveLength(CHAT_KEEP);
    expect(held[0]?.seq).toBe(4);
  });

  test('the driver and the requests are kept for each session', () => {
    const state = apply([
      { type: 'driver', ...key, driver: 'member:Ana' },
      { type: 'drive_request', ...key, requests: ['member:Bo'] },
    ]);
    expect(state.driver[id]).toBe('member:Ana');
    expect(state.driveRequests[id]).toEqual(['member:Bo']);
    const released = apply([{ type: 'driver', ...key, driver: null }], state);
    expect(released.driver[id]).toBeNull();
    expect(released.driveRequests[id]).toEqual(['member:Bo']);
  });

  test('a new deck incarnation clears every people answer', () => {
    const first = apply([
      { type: 'hello', protocol: 1, deck_incarnation: 'one' },
      { type: 'presence', ...key, who: ['owner'] },
      { type: 'chat', ...key, message: chat(1) },
      { type: 'driver', ...key, driver: 'owner' },
      { type: 'drive_request', ...key, requests: ['member:Bo'] },
    ]);
    // The same deck after a reconnect keeps them.
    const same = reduceMessage(first, { type: 'hello', protocol: 1, deck_incarnation: 'one' });
    expect(same.presence[id]).toEqual(['owner']);
    const next = reduceMessage(first, { type: 'hello', protocol: 1, deck_incarnation: 'two' });
    expect(next.deckIncarnation).toBe('two');
    expect(next.presence).toEqual({});
    expect(next.chat).toEqual({});
    expect(next.driver).toEqual({});
    expect(next.driveRequests).toEqual({});
  });

  test('the first hello keeps what the page holds', () => {
    const state = apply([
      { type: 'presence', ...key, who: ['owner'] },
      { type: 'hello', protocol: 1, deck_incarnation: 'one' },
    ]);
    expect(state.presence[id]).toEqual(['owner']);
  });
});
