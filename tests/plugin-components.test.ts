// Contract coverage for the Claude Code-facing plugin surface. These checks
// keep quiet argv-based hook execution, timeout policy, manifest consistency, and
// the user-visible command/skill guidance from drifting independently.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

interface HookCommand {
  type?: unknown;
  command?: unknown;
  args?: unknown;
  timeout?: unknown;
  async?: unknown;
  asyncRewake?: unknown;
}

interface HookGroup {
  hooks?: HookCommand[];
}

interface HooksFile {
  hooks?: Record<string, HookGroup[]>;
}

function hooksFile(): HooksFile {
  return JSON.parse(readFileSync(path.join(ROOT, 'hooks/hooks.json'), 'utf8')) as HooksFile;
}

function commandsFor(event: string): HookCommand[] {
  return (hooksFile().hooks?.[event] ?? []).flatMap((group) => group.hooks ?? []);
}

test('plugin hooks use the quiet argv launcher and only the documented top-level hooks key', () => {
  const parsed = hooksFile();
  assert.deepEqual(Object.keys(parsed), ['hooks']);
  const hooks = parsed.hooks;
  assert.ok(hooks);
  assert.deepEqual(
    Object.keys(hooks).sort(),
    [
      'CwdChanged',
      'Elicitation',
      'Notification',
      'PermissionRequest',
      'PostToolUse',
      'PostToolUseFailure',
      'PreToolUse',
      'SessionEnd',
      'SessionStart',
      'Stop',
      'UserPromptSubmit',
    ].sort(),
  );

  for (const [event, groups] of Object.entries(hooks)) {
    for (const entry of groups.flatMap((group) => group.hooks ?? [])) {
      assert.equal(entry.type, 'command', `${event} must remain a command hook`);
      assert.equal(entry.command, '/bin/sh', `${event} must use the portable quiet launcher`);
      assert.ok(Array.isArray(entry.args) && entry.args.length >= 3, `${event} must declare argv`);
      const [launcher, mode, target] = entry.args as unknown[];
      assert.equal(
        launcher,
        '${CLAUDE_PLUGIN_ROOT}/scripts/hook-launcher.sh',
        `${event} must resolve the launcher through CLAUDE_PLUGIN_ROOT`,
      );
      assert.ok(
        typeof target === 'string' && target.startsWith('${CLAUDE_PLUGIN_ROOT}/scripts/'),
        `${event} must resolve its bundle through CLAUDE_PLUGIN_ROOT`,
      );
      const expectedMode = target.endsWith('/fleet-sessionstart.mjs')
        ? 'sessionstart'
        : target.endsWith('/fleet-watch.mjs')
          ? 'watch'
          : 'decision';
      assert.equal(mode, expectedMode, `${event} must select the matching silence policy`);
    }
  }
});

test('interactive holds and lifecycle hooks keep their fail-open timeout policy', () => {
  for (const event of ['AskUserQuestion', 'PermissionRequest', 'Elicitation']) {
    const sourceEvent = event === 'AskUserQuestion' ? 'PreToolUse' : event;
    const entry = commandsFor(sourceEvent).find(
      (hook) => Array.isArray(hook.args) && hook.args.includes(event),
    );
    assert.ok(entry, `${event} relay hook is registered`);
    assert.equal(entry.timeout, 720, `${event} must stay above the 660s board watchdog`);
  }

  const [sessionEnd] = commandsFor('SessionEnd');
  assert.ok(sessionEnd);
  assert.equal(sessionEnd.timeout, 1, "SessionEnd must fit Claude Code's short shutdown budget");
  assert.equal(sessionEnd.async, true, 'SessionEnd telemetry must never block exit');

  const watcher = commandsFor('Stop').find((hook) => hook.asyncRewake === true);
  assert.ok(watcher, 'Stop must retain its asyncRewake mail watcher');
  assert.ok(Number(watcher.timeout) > 24 * 3600, 'watcher timeout exceeds its 24h lifetime cap');
});

test('plugin and marketplace metadata stay identical', () => {
  const plugin = JSON.parse(
    readFileSync(path.join(ROOT, '.claude-plugin/plugin.json'), 'utf8'),
  ) as { version?: unknown; description?: unknown };
  const marketplace = JSON.parse(
    readFileSync(path.join(ROOT, '.claude-plugin/marketplace.json'), 'utf8'),
  ) as { plugins?: { version?: unknown; description?: unknown }[] };
  const listing = marketplace.plugins?.[0];
  assert.ok(listing);
  assert.equal(listing.version, plugin.version);
  assert.equal(listing.description, plugin.description);
});

test('fleet doctrine documents native terminal prompts and bounds every curl', () => {
  const skill = readFileSync(path.join(ROOT, 'skills/fleet-doctrine/SKILL.md'), 'utf8');
  assert.match(skill, /ordinary terminal session[\s\S]*native prompt/);
  assert.doesNotMatch(skill, /User answered via Fleet Deck:/);
  for (const line of skill.split('\n').filter((candidate) => /\bcurl\b/.test(candidate))) {
    assert.match(line, /--max-time 3/, `unbounded fleet call in skill: ${line}`);
  }
});
