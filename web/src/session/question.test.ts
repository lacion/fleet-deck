import { describe, expect, test } from 'bun:test';
import type { OpenRequest } from '../gen';
import { recordedRequest } from '../testing/timeline';
import { sessionActions } from './commands';
import { complete, questionForm } from './question';

describe('question answers (DL-068)', () => {
  test('a Codex requestUserInput gets the recorded answer shape', () => {
    const form = questionForm(recordedRequest('codex-question_rpc'));
    expect(form).not.toBeNull();
    if (!form) return;
    expect(form.submit).toBe('answer');
    expect(form.others).toEqual([]);
    expect(form.questions).toEqual([
      {
        id: 'preferred_color',
        header: 'Color',
        text: 'Which color do you prefer?',
        options: ['Red', 'Blue'],
        other: true,
        multi: false,
        secret: false,
      },
    ]);
    // The answer that the recording sent (spikes/s2-codex/fixtures/question_rpc.jsonl:36).
    expect(form.build({ preferred_color: ['blue'] })).toEqual({
      updated_input: { answers: { preferred_color: { answers: ['blue'] } } },
      text: null,
    });
    expect(complete(form, {})).toBe(false);
    expect(complete(form, { preferred_color: ['  '] })).toBe(false);
    expect(complete(form, { preferred_color: ['Blue'] })).toBe(true);
  });

  test('a Claude AskUserQuestion gets the recorded answer shape', () => {
    const request = recordedRequest('ask_user_question');
    const form = questionForm(request);
    expect(form).not.toBeNull();
    if (!form) return;
    expect(form.submit).toBe('allow');
    expect(form.others).toEqual(['deny']);
    expect(form.questions.map((q) => [q.id, q.options, q.other])).toEqual([
      ['Which color?', ['red', 'blue'], true],
    ]);
    // The answer that the recording sent
    // (spikes/s1-claude/fixtures/rust/ask_user_question.jsonl:51).
    expect(form.build({ 'Which color?': ['blue'] })).toEqual({
      updated_input: {
        answers: { 'Which color?': 'blue' },
        questions: [
          {
            header: 'Color',
            multiSelect: false,
            options: [
              { description: 'The color red', label: 'red' },
              { description: 'The color blue', label: 'blue' },
            ],
            question: 'Which color?',
          },
        ],
      },
      text: null,
    });
  });

  test('a Codex message question answers with text; other requests keep their buttons', () => {
    const base = recordedRequest('codex-question_rpc');
    const message: OpenRequest = {
      ...base,
      response: 'message',
      input: { title: 'Pick one', options: ['red', 'blue'] },
      options: [
        { option_id: 'red', label: 'red' },
        { option_id: 'blue', label: 'blue' },
        { option_id: 'answer', label: 'Answer' },
      ],
    };
    const form = questionForm(message);
    expect(form?.questions.map((q) => [q.text, q.options])).toEqual([
      ['Pick one', ['red', 'blue']],
    ]);
    expect(form?.build({ answer: ['green'] })).toEqual({ updated_input: null, text: 'green' });
    expect(questionForm({ ...base, request_kind: 'command' })).toBeNull();
    expect(questionForm({ ...base, input: null })).toBeNull();
    expect(questionForm({ ...base, input: { questions: [] } })).toBeNull();
  });

  test('the answer command carries the reply', async () => {
    const sent: unknown[] = [];
    const actions = sessionActions(
      async (_path, body) => {
        sent.push(body);
        return { state: 'done', receipt: null };
      },
      { station_id: 'st', session_id: 's1' },
      ['approve'],
      () => 'b1',
    );
    await actions.answer('req-1', 'answer', {
      updated_input: { answers: { c: { answers: ['Blue'] } } },
      text: null,
    });
    await actions.answer('req-2', 'accept');
    expect(sent).toEqual([
      {
        station: 'st',
        command_id: 'b1',
        command: {
          op: 'answer',
          session_id: 's1',
          request_id: 'req-1',
          decision: {
            option_id: 'answer',
            updated_input: { answers: { c: { answers: ['Blue'] } } },
            text: null,
          },
        },
      },
      {
        station: 'st',
        command_id: 'b1',
        command: {
          op: 'answer',
          session_id: 's1',
          request_id: 'req-2',
          decision: { option_id: 'accept', updated_input: null, text: null },
        },
      },
    ]);
  });
});
