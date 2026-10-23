import { describe, expect, test } from 'bun:test';
import type { Item } from '../session/timeline';
import {
  agentLine,
  answeredText,
  apiCause,
  errorNotice,
  expiredText,
  modelNotice,
  modeNotice,
  pairTurnErrors,
  resolvedByText,
  terminalReturnNotice,
} from './notices';

describe('errorNotice', () => {
  test('delivery_unknown says what may have happened, with no code, id or reason id', () => {
    const message =
      'the station cannot tell whether input 01M5KXDYADTF7EEG7DYNPJSG1X reached the provider (process_lost); it does not send it again';
    expect(errorNotice('delivery_unknown', message)).toEqual({
      text: 'A message may not have reached the agent: the agent process stopped. The station does not send it again.',
      title: `delivery_unknown: ${message}`,
    });
    // The first input, after a station restart.
    expect(
      errorNotice(
        'delivery_unknown',
        'the station cannot tell whether the first input 01M5KXDYADTF7EEG7DYNPJSG1X reached the provider (station_restart); it does not send it again',
      ).text,
    ).toBe(
      'A message may not have reached the agent: the station restarted. The station does not send it again.',
    );
    // A command (no input), and a reason without words.
    expect(
      errorNotice(
        'delivery_unknown',
        'the station cannot tell whether interrupt reached the provider (shim_write_unknown)',
      ).text,
    ).toBe('A command may not have reached the agent.');
  });

  test('any other code shows its message as a sentence, without the code and the input ids', () => {
    expect(
      errorNotice(
        'first_input_not_sent',
        'the session started, but its first input 01M5KXDYADTF7EEG7DYNPJSG1X was not sent: the shim refused it',
      ),
    ).toEqual({
      text: 'The session started, but its first input was not sent: the shim refused it.',
      title:
        'first_input_not_sent: the session started, but its first input 01M5KXDYADTF7EEG7DYNPJSG1X was not sent: the shim refused it',
    });
    expect(errorNotice('start_failed', 'Claude is not signed in.').text).toBe(
      'Claude is not signed in.',
    );
    expect(errorNotice('start_failed', '')).toEqual({
      text: 'Start failed.',
      title: 'start_failed',
    });
  });
});

describe('the outcome of a request', () => {
  test('an expired request says why in words', () => {
    expect(expiredText('process_lost')).toBe('expired: the agent process stopped');
    expect(expiredText('turn_ended')).toBe('expired: the turn ended');
    expect(expiredText('not_resumable')).toBe('expired: the session cannot resume');
    expect(expiredText('cancelled_by_provider')).toBe('withdrawn by the agent');
    expect(expiredText('something_new')).toBe('expired');
  });

  test('an answered request names who answered it, unless it was you on the board', () => {
    expect(resolvedByText('board')).toBe('');
    expect(resolvedByText('policy')).toBe('by a rule');
    expect(resolvedByText('timeout')).toBe('after the time limit');
    expect(resolvedByText('coordinator')).toBe('by the coordinator');
    expect(resolvedByText('local')).toBe('in the terminal');
    expect(answeredText('Allow once', 'board')).toBe('Allow once');
    expect(answeredText('Allow once', 'policy')).toBe('Allow once, by a rule');
    expect(answeredText('red', 'local')).toBe('red, in the terminal');
  });
});

describe('the session notices', () => {
  test('a model change names the model and its context, the raw id in the title', () => {
    expect(modelNotice('claude-opus-5-5[1m]')).toEqual({
      text: 'Model changed to Opus 5.5 (1M context)',
      title: 'claude-opus-5-5[1m]',
    });
    expect(modelNotice('claude-sonnet-5-5').text).toBe('Model changed to Sonnet 5.5');
  });

  test('a mode change has the one name of the mode', () => {
    expect(modeNotice('bypassPermissions')).toEqual({
      text: 'Mode: Bypass permissions',
      title: 'bypassPermissions',
    });
    expect(modeNotice('on-request/workspace-write').text).toBe(
      'Mode: Ask when needed · workspace write',
    );
  });

  test('a terminal return says why in words, or nothing', () => {
    expect(terminalReturnNotice('the terminal process exited (code 1)')).toEqual({
      text: 'Returned to board control: the terminal closed.',
      title: 'the terminal process exited (code 1)',
    });
    expect(terminalReturnNotice('tui exited').text).toBe(
      'Returned to board control: the terminal closed.',
    );
    expect(terminalReturnNotice('the station restarted; the terminal process is gone').text).toBe(
      'Returned to board control: the station restarted.',
    );
    expect(terminalReturnNotice('the terminal did not start: no tty').text).toBe(
      'Returned to board control: the terminal did not start.',
    );
    expect(terminalReturnNotice('something else')).toEqual({
      text: 'Returned to board control.',
      title: 'something else',
    });
    expect(terminalReturnNotice(undefined)).toEqual({ text: 'Returned to board control.' });
  });
});

describe('a failed turn', () => {
  test('the cause of a provider error in words', () => {
    expect(
      apiCause(
        'API Error: Request rejected (429) · All credentials for model claude-opus-5-5 are cooling down',
      ),
    ).toBe("The provider's rate limit stopped it. Try again later.");
    expect(apiCause('API Error: 529 {"type":"overloaded_error"}')).toBe(
      'The provider is overloaded. Try again later.',
    );
    expect(apiCause('API Error: 401 {"type":"authentication_error"}')).toBe(
      'The provider refused the sign-in.',
    );
    expect(apiCause('API Error: 500 Internal server error')).toBe(
      'The provider had an error. Try again later.',
    );
    expect(apiCause('API Error: something')).toBe('');
  });

  test('a failed turn takes its last agent text when it is a provider error', () => {
    const items: Item[] = [
      { type: 'run', key: 'r1', run_id: 'r1', input: 'go', origin: 'board' },
      { type: 'message', key: 'm1', text: 'Working on it.', done: true },
      { type: 'message', key: 'm2', text: 'API Error: Request rejected (429)', done: true },
      { type: 'turn', key: 't1', status: 'failed', turn_id: 't1', diff: null },
      { type: 'run', key: 'r2', run_id: 'r2', input: 'again', origin: 'board' },
      { type: 'message', key: 'm3', text: 'API Error: shown as prose', done: true },
      { type: 'turn', key: 't2', status: 'interrupted', turn_id: 't2', diff: null },
      { type: 'message', key: 'm4', text: 'Plain text', done: true },
      { type: 'turn', key: 't3', status: 'failed', turn_id: 't3', diff: null },
    ];
    const pairs = pairTurnErrors(items);
    expect([...pairs.hidden]).toEqual(['m2']);
    expect([...pairs.detail]).toEqual([['t1', 'API Error: Request rejected (429)']]);
  });
});

describe('agentLine', () => {
  test('a provider error reads as its cause in words', () => {
    expect(agentLine('API Error: Request rejected (429) · All credentials are cooling down')).toBe(
      "The provider's rate limit stopped it. Try again later.",
    );
  });

  test('a provider error with no known cause says that the provider refused the turn', () => {
    expect(agentLine('API Error: something odd')).toBe('The provider refused the turn.');
  });

  test('another text stays', () => {
    expect(agentLine('Working on it.')).toBe('Working on it.');
    expect(agentLine('The API Error: is in the middle')).toBe('The API Error: is in the middle');
  });

  test('spaces before "API Error:" are an error too', () => {
    expect(agentLine('  \nAPI Error: 529 overloaded')).toBe(
      'The provider is overloaded. Try again later.',
    );
  });
});
