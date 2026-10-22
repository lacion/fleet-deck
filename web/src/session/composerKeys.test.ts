// §4.5: what Enter and Ctrl/⌘+Enter send from the composer text box (#fd-composer-text). The
// composer's onKeyDown sends `enterSend(...)` with `steerable = canSteer(...)` of its props.
import { describe, expect, test } from 'bun:test';
import { canSteer, enterSend, inputOf, type SendKey } from './Composer';

const press = (extra: Partial<SendKey> = {}): SendKey => ({
  key: 'Enter',
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  ...extra,
});

/** The composer's state: `text` with `bang` on (the `!` shell mode), a fine pointer. */
const at = (props: { working: boolean; steer: boolean }, text = 'look at the logs') => ({
  working: props.working,
  steerable: canSteer(props.working, props.steer, inputOf(text, true).kind === 'shell'),
  coarse: false,
});

describe('composer send keys (§4.5)', () => {
  test('Ctrl+Enter or ⌘+Enter steers while a turn runs and the provider can steer', () => {
    const running = at({ working: true, steer: true });
    expect(enterSend(press({ ctrlKey: true }), running)).toBe('steer');
    expect(enterSend(press({ metaKey: true }), running)).toBe('steer');
    // A plain Enter still queues the follow-up.
    expect(enterSend(press(), running)).toBe('follow_up');
  });

  test('without steer support Ctrl+Enter queues the follow-up, as Enter does', () => {
    const running = at({ working: true, steer: false });
    expect(enterSend(press({ ctrlKey: true }), running)).toBe('follow_up');
    expect(enterSend(press(), running)).toBe('follow_up');
  });

  test('a ! shell command never steers', () => {
    const running = at({ working: true, steer: true }, '!cargo test');
    expect(running.steerable).toBe(false);
    expect(enterSend(press({ ctrlKey: true }), running)).not.toBe('steer');
  });

  test('with no turn running Enter and Ctrl+Enter send the prompt', () => {
    const idle = at({ working: false, steer: true });
    expect(enterSend(press(), idle)).toBe('prompt');
    expect(enterSend(press({ ctrlKey: true }), idle)).toBe('prompt');
  });

  test('Shift+Enter, Enter on a touch screen and other keys send nothing', () => {
    const running = at({ working: true, steer: true });
    expect(enterSend(press({ shiftKey: true }), running)).toBeNull();
    expect(enterSend(press(), { ...running, coarse: true })).toBeNull();
    expect(enterSend(press({ key: 'a', ctrlKey: true }), running)).toBeNull();
    // Ctrl+Enter sends also on a touch screen (a hardware keyboard).
    expect(enterSend(press({ ctrlKey: true }), { ...running, coarse: true })).toBe('steer');
  });
});
