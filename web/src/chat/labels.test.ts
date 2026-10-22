import { describe, expect, test } from 'bun:test';
import { argText, durationText, homeTilde, thinkingText, toolName } from './labels';

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

  test('any other title stays as it is', () => {
    expect(toolName('Bash')).toEqual({ name: 'Bash', server: null });
    expect(toolName('mcp__fleetdeck')).toEqual({ name: 'mcp__fleetdeck', server: null });
    expect(toolName('mcp__fleetdeck__')).toEqual({ name: 'mcp__fleetdeck__', server: null });
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
      argText(`{"name":"audit","script":"export const meta = {\\n  name: 'x',\\n  desc…`),
    ).toBe("audit · export const meta = { name: 'x', desc…");
    // A cut in the middle of an escape drops the broken escape.
    expect(argText('{"a":"one\\…')).toBe('one…');
    // A cut in a key, or right after it, adds nothing.
    expect(argText('{"a":"one","descri…')).toBe('one');
    expect(argText('{"a":"one","b":…')).toBe('one');
  });

  test('any other input is its first line', () => {
    expect(argText('ls -la\ncd /tmp')).toBe('ls -la');
    expect(argText('{not json')).toBe('{not json');
    expect(argText('{"nested":{"a":1}}')).toBe('{"nested":{"a":1}}');
    expect(argText('')).toBe('');
  });
});

describe('homeTilde', () => {
  test('every home folder of Linux or macOS reads as ~', () => {
    expect(homeTilde('cat /home/me/notes/a.md > /Users/me/b.md')).toBe('cat ~/notes/a.md > ~/b.md');
    expect(homeTilde('/home/me/x')).toBe('~/x');
    expect(homeTilde('open "/Users/me/a b/c"')).toBe('open "~/a b/c"');
  });

  test('a home folder inside another path, or without a path after it, stays', () => {
    expect(homeTilde('/srv/home/me/x')).toBe('/srv/home/me/x');
    expect(homeTilde('ls /home/me')).toBe('ls /home/me');
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
