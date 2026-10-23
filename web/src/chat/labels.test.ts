import { describe, expect, test } from 'bun:test';
import {
  agentFile,
  argText,
  BUILTIN_WORDS,
  durationText,
  requestTitle,
  thinkingText,
  toolName,
} from './labels';

describe('agentFile', () => {
  test('a path in the folder of the agent CLI is its file name; any other value is null', () => {
    expect(agentFile('/home/me/.claude/projects/-home-me-x/0549/workflows/fix.js')).toBe('fix.js');
    expect(agentFile('~/.claude/agents/review.md')).toBe('review.md');
    expect(agentFile('/Users/ana/.claude/x')).toBe('x');
    expect(agentFile('/home/me/app/src/a.ts')).toBeNull();
    expect(agentFile('~/.claude/a b')).toBeNull();
    expect(agentFile('wf_b7605b1a-452')).toBeNull();
  });
});

describe('toolName', () => {
  test('an MCP tool reads as its tool in sentence case, with its server apart', () => {
    expect(toolName('mcp__fleetdeck__publish_artifact')).toEqual({
      name: 'Publish artifact',
      server: 'fleetdeck',
    });
    expect(toolName('mcp__github__list-issues')).toEqual({ name: 'List issues', server: 'github' });
    // The server ends at the first `__`.
    expect(toolName('mcp__plugin_x__take_screenshot')).toEqual({
      name: 'Take screenshot',
      server: 'plugin_x',
    });
  });

  test('a built-in tool whose id is not a word reads as its word', () => {
    expect(toolName('AskUserQuestion')).toEqual({ name: 'Question', server: null });
    expect(toolName('ExitPlanMode')).toEqual({ name: 'Plan', server: null });
    expect(toolName('TodoWrite')).toEqual({ name: 'To-do list', server: null });
    expect(toolName('KillShell')).toEqual({ name: 'Stop command', server: null });
    expect(BUILTIN_WORDS['WebSearch']).toBe('Web search');
  });

  test('any other title stays as it is', () => {
    expect(toolName('Bash')).toEqual({ name: 'Bash', server: null });
    expect(toolName('mcp__fleetdeck')).toEqual({ name: 'mcp__fleetdeck', server: null });
    expect(toolName('mcp__fleetdeck__')).toEqual({ name: 'mcp__fleetdeck__', server: null });
  });
});

describe('requestTitle', () => {
  test('a built-in tool id at the start of a request title reads as its word', () => {
    expect(requestTitle('AskUserQuestion')).toBe('Question');
    expect(requestTitle('ExitPlanMode the plan')).toBe('Plan the plan');
    expect(requestTitle('Bash rm -rf dist')).toBe('Bash rm -rf dist');
    expect(requestTitle('AskUserQuestionnaire')).toBe('AskUserQuestionnaire');
  });
});

describe('argText', () => {
  test('a JSON object reads as its plain values', () => {
    expect(argText('{"task_id":"bv93ogg6d"}')).toBe('bv93ogg6d');
    expect(argText('{"path":"/a/b.md","pages":2,"all":true,"opts":{"x":1}}')).toBe(
      '/a/b.md · 2 · true',
    );
    expect(argText('{\n  "command": "ls\\n  -la"\n}')).toBe('ls -la');
    expect(argText('{"a":null,"b":"say \\"hi\\" {now}","c":[1,"x"],"d":-2.5}')).toBe(
      'say "hi" {now} · -2.5',
    );
  });

  test('a JSON object that the station cut at 200 characters reads as the values before the cut', () => {
    expect(
      argText(`{"args":{"n":1},"script":"export const meta = {\\n  name: 'x',\\n  desc…`),
    ).toBe("export const meta = { name: 'x', desc…");
    // A cut in the middle of an escape drops the broken escape.
    expect(argText('{"a":"one\\…')).toBe('one…');
    // A cut in a key, or right after it, adds nothing.
    expect(argText('{"a":"one","descri…')).toBe('one');
    expect(argText('{"a":"one","b":…')).toBe('one');
  });

  test('a description, a name or a title names the call: the other values go', () => {
    expect(
      argText(`{"name":"audit","script":"export const meta = {\\n  name: 'x',\\n  desc…`),
    ).toBe('audit');
    expect(argText('{"content":"# Report","kind":"report","title":"Weekly audit"}')).toBe(
      'Weekly audit',
    );
    // A description comes before a name, a name before a title.
    expect(argText('{"description":"Fix the flaky test","name":"fix","title":"T"}')).toBe(
      'Fix the flaky test',
    );
    expect(argText('{"title":"T","name":"fix"}')).toBe('fix');
    // A name inside a nested object does not count.
    expect(argText('{"args":{"name":"x"},"id":"w1"}')).toBe('w1');
  });

  test('a path in the folder of the agent (~/.claude) reads as its file name', () => {
    // A Workflow call: the script that the session wrote under its transcript folder.
    expect(
      argText(
        '{"args":{"phase":2},"scriptPath":"/home/me/.claude/projects/-home-me-projects-reggio/05492400-1b42-48a1-86f0-5b1e2c3d4e5f/workflows/fix-schema.js"}',
      ),
    ).toBe('fix-schema.js');
    expect(argText('{"path":"/Users/ana/.claude/agents/review.md","depth":2}')).toBe(
      'review.md · 2',
    );
    // A path elsewhere stays whole.
    expect(argText('{"path":"/home/me/app/src/a.ts"}')).toBe('/home/me/app/src/a.ts');
  });

  test('a question one level down names an AskUserQuestion call; the count follows', () => {
    const one =
      '{"questions":[{"header":"Color","multiSelect":false,"options":[{"description":"The color red","label":"red"}],"question":"Which color?"}]}';
    expect(argText(one)).toBe('Which color?');
    // The station cut the input before the question: its header names it.
    expect(argText('{"questions":[{"header":"Reset data","multiSelect":false,"options"…')).toBe(
      'Reset data',
    );
    const two =
      '{"questions":[{"header":"A","question":"First?"},{"header":"B","question":"Second?"}]}';
    expect(argText(two)).toBe('First? · 2 questions');
    // A label of an option is not a question.
    expect(argText('{"questions":[{"options":[{"label":"red"}]}]}')).toBe('');
  });

  test('a JSON input without a value to show is empty, never JSON text', () => {
    expect(argText('{not json')).toBe('');
    expect(argText('{"nested":{"a":1}}')).toBe('');
    expect(argText('{"list":["a","b"]}')).toBe('');
  });

  test('any other input is its first line', () => {
    expect(argText('ls -la\ncd /tmp')).toBe('ls -la');
    expect(argText('')).toBe('');
  });
});

describe('Thinking', () => {
  test('durations in the words of the board', () => {
    expect(durationText(300)).toBe('1 s');
    expect(durationText(12_400)).toBe('12 s');
    expect(durationText(125_000)).toBe('2 min');
    expect(durationText(2 * 3_600_000 + 60_000)).toBe('2 h');
  });

  test('the summary says how long it thought, once it is done', () => {
    expect(thinkingText({ done: false, started_at: 0, ended_at: 9_000 })).toBe('Thinking…');
    expect(thinkingText({ done: true, started_at: 1_000, ended_at: 13_000 })).toBe(
      'Thinking · 12 s',
    );
    // One delta, or no times: the span is not known.
    expect(thinkingText({ done: true, started_at: 5, ended_at: 5 })).toBe('Thinking');
    expect(thinkingText({ done: true })).toBe('Thinking');
  });
});
