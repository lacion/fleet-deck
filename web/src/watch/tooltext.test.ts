// The words of a tool call: no raw MCP id, no JSON payload, no "(ok)".
import { describe, expect, test } from 'bun:test';
import { activityTool, homePath, toolLabel, toolTarget, toolText, toolWords } from './tooltext';

describe('toolLabel', () => {
  test('an MCP id gives the tool in words and the server after a dot', () => {
    expect(toolLabel('mcp__github__getIssue')).toBe('Get issue · github');
    expect(toolLabel('mcp__plugin_linear_linear__list_issues')).toBe('List issues · linear_linear');
    expect(toolLabel('mcp__chrome-devtools__take_screenshot')).toBe(
      'Take screenshot · chrome-devtools',
    );
  });

  test("a tool of the deck's own server needs no server name", () => {
    expect(toolLabel('mcp__fleetdeck__publish_artifact')).toBe('Publish artifact');
    expect(toolLabel('mcp__fleetdeck__fleet_view')).toBe('Fleet view');
  });

  test('the short name of a tile line has no server', () => {
    expect(toolWords('mcp__chrome-devtools__take_screenshot')).toBe('Take screenshot');
    expect(toolWords('mcp__fleetdeck__publish_artifact')).toBe('Publish artifact');
    expect(toolWords('Bash')).toBe('Bash');
  });

  test('every other name stays as it is', () => {
    expect(toolLabel('Read')).toBe('Read');
    expect(toolLabel('TaskStop')).toBe('TaskStop');
    expect(toolLabel('exec_command')).toBe('exec_command');
  });
});

describe('toolTarget', () => {
  test('a JSON input gives its first known field, never the JSON text', () => {
    expect(toolTarget('{"description":"Fix the shim","prompt":"Long…"}')).toEqual({
      target: 'Fix the shim',
      form: 'words',
    });
    expect(toolTarget('{"file_path":"/w/a.ts","old_string":"x"}')).toEqual({
      target: '/w/a.ts',
      form: 'path',
    });
    expect(toolTarget('{"command":"cargo test\\n--all"}')).toEqual({
      target: 'cargo test',
      form: 'code',
    });
  });

  test('a name or a title comes before a command; a path under ~/.claude/ is its file name', () => {
    expect(toolTarget('{"command":"make","name":"Build"}')).toEqual({
      target: 'Build',
      form: 'words',
    });
    expect(
      toolTarget(
        '{"resumeFromRunId":"wf_b7605b1a-452","scriptPath":"/home/me/.claude/projects/-home-me-x/0549/workflows/reggio-db-fixes.js"}',
      ),
    ).toEqual({ target: 'reggio-db-fixes.js', form: 'path' });
    expect(toolTarget('{"path":"/home/me/.claude/agents/review.md"}')).toEqual({
      target: 'review.md',
      form: 'path',
    });
  });

  test('an id is the target when no known field is there', () => {
    expect(toolTarget('{"task_id":"bv93ogg6d"}')).toEqual({ target: 'bv93ogg6d', form: 'code' });
  });

  test('a JSON input without a known field, or a cut one, has no target', () => {
    expect(toolTarget('{"script":"export const meta = {\\n name')).toEqual({
      target: '',
      form: 'words',
    });
    expect(toolTarget('{"view":"sessions"}')).toEqual({ target: '', form: 'words' });
  });

  test('a cut JSON input still gives its complete fields', () => {
    expect(toolTarget('{"description":"Build the DAG","script":"export const')).toEqual({
      target: 'Build the DAG',
      form: 'words',
    });
  });

  test('any other input gives its first line; a lone absolute path is a path', () => {
    expect(toolTarget('git status\ngit log')).toEqual({ target: 'git status', form: 'words' });
    expect(toolTarget('/Users/ana/notes.md')).toEqual({
      target: '/Users/ana/notes.md',
      form: 'path',
    });
  });
});

describe('toolText', () => {
  test('a read or edit call, or one file that is the input, is a path', () => {
    expect(toolText('Read', '/w/a.ts', { kind: 'read' }).form).toBe('path');
    expect(toolText('Tool', 'src/a.ts', { kind: 'other', files: ['src/a.ts'] }).form).toBe('path');
    expect(toolText('Edit', 'src/a.ts').form).toBe('path');
  });

  test('a command is code, a prose input is words', () => {
    expect(toolText('Bash', 'cargo test', { kind: 'shell' })).toEqual({
      name: 'Bash',
      target: 'cargo test',
      form: 'code',
    });
    expect(toolText('Agent', 'Review the diff', { kind: 'subagent' }).form).toBe('words');
  });

  test('the shell tools that watch or stop a command read as code', () => {
    expect(toolText('Monitor', 'sleep 1700; echo tick').form).toBe('code');
    expect(toolText('BashOutput', 'bash_3').form).toBe('code');
    expect(toolText('KillShell', 'bash_3').form).toBe('code');
  });

  test('an MCP call with a JSON input', () => {
    expect(toolText('mcp__fleetdeck__fleet_view', '{"view":"sessions"}')).toEqual({
      name: 'Fleet view',
      target: '',
      form: 'words',
    });
  });
});

describe('homePath', () => {
  test('the home folder of a user becomes ~/', () => {
    expect(homePath('/home/ana/briefs/a.png')).toBe('~/briefs/a.png');
    expect(homePath('/Users/ana/x')).toBe('~/x');
    expect(homePath('/srv/home/ana/x')).toBe('/srv/home/ana/x');
  });
});

describe('activityTool', () => {
  test('a finished call loses its "(ok)"; failed and denied are the outcome', () => {
    expect(activityTool('tool', 'Read: /w/a.ts (ok)')).toEqual({
      name: 'Read',
      target: '/w/a.ts',
      form: 'path',
      lead: '',
      outcome: null,
    });
    expect(activityTool('tool', 'Bash: head -40 (failed)')).toMatchObject({
      name: 'Bash',
      target: 'head -40',
      form: 'code',
      outcome: 'failed',
    });
    expect(activityTool('tool', 'Write: /w/b.ts (denied)')?.outcome).toBe('denied');
  });

  test('a JSON input becomes its field; an MCP name becomes words', () => {
    expect(activityTool('tool', 'TaskStop: {"task_id":"bv93ogg6d"} (ok)')).toMatchObject({
      name: 'TaskStop',
      target: 'bv93ogg6d',
      form: 'code',
    });
    expect(activityTool('tool', 'mcp__fleetdeck__publish_artifact (ok)')).toMatchObject({
      name: 'Publish artifact',
      target: '',
    });
  });

  test('a Monitor command is code, the same as a Bash command', () => {
    expect(activityTool('tool', 'Monitor: sleep 1700; echo tick (ok)')).toMatchObject({
      name: 'Monitor',
      target: 'sleep 1700; echo tick',
      form: 'code',
    });
  });

  test('a call without an input, or without a title', () => {
    expect(activityTool('tool', 'TaskList (ok)')).toMatchObject({ name: 'TaskList', target: '' });
    expect(activityTool('tool', 'Tool call (failed)')).toMatchObject({
      name: 'Tool call',
      target: '',
      outcome: 'failed',
    });
  });

  test('a request row splits only for a known tool and keeps its lead', () => {
    expect(activityTool('approval', 'Permission: Bash cargo test')).toMatchObject({
      lead: 'Permission:',
      name: 'Bash',
      target: 'cargo test',
      form: 'code',
    });
    expect(activityTool('approval', 'Needs you: Pick a database')).toBeNull();
    expect(activityTool('approval', 'Question: Which port?')).toBeNull();
  });

  test('a message is words only', () => {
    expect(activityTool('message', 'Read: the docs')).toBeNull();
  });
});
