#!/usr/bin/env node
// demo/render-smoke-settings.mjs — render the smoke workers'
// .claude/settings.json through JSON.stringify instead of a shell heredoc, so
// a checkout path that contains a quote or backslash can never corrupt the
// generated JSON (BUG-189). Usage:
//
//   node render-smoke-settings.mjs <sessionstart.mjs> <fleet-hook.mjs> <out-file>
//
// Every hook keeps using the current checkout's authenticated command shim —
// native HTTP hooks cannot attach the bearer token required since 0.16.0.

import { writeFileSync } from 'node:fs';

const [sessionStartScript, fleetHookScript, outFile] = process.argv.slice(2);
if (!sessionStartScript || !fleetHookScript || !outFile) {
  console.error('usage: node render-smoke-settings.mjs <sessionstart.mjs> <fleet-hook.mjs> <out-file>');
  process.exit(2);
}

// The hook commands run through a shell, so quote the script paths for the
// shell; the JSON layer itself is handled by JSON.stringify below.
const shellQuote = (value) => `'${String(value).replace(/'/g, `'\\''`)}'`;

const sessionStart = `bun ${shellQuote(sessionStartScript)}`;
const hook = (event) => `bun ${shellQuote(fleetHookScript)} ${event}`;

const settings = {
  hooks: {
    SessionStart: [
      { hooks: [{ type: 'command', command: sessionStart, timeout: 15 }] },
    ],
    UserPromptSubmit: [
      { hooks: [{ type: 'command', command: hook('UserPromptSubmit'), timeout: 3 }] },
    ],
    PostToolUse: [
      { matcher: 'Edit|Write|MultiEdit|NotebookEdit|Bash', hooks: [{ type: 'command', command: hook('PostToolUse'), timeout: 3 }] },
    ],
    PreToolUse: [
      { matcher: 'AskUserQuestion', hooks: [{ type: 'command', command: hook('AskUserQuestion'), timeout: 65 }] },
    ],
    PermissionRequest: [
      { hooks: [{ type: 'command', command: hook('PermissionRequest'), timeout: 65 }] },
    ],
    Elicitation: [
      { hooks: [{ type: 'command', command: hook('Elicitation'), timeout: 65 }] },
    ],
    Notification: [
      { hooks: [{ type: 'command', command: hook('Notification'), timeout: 3, async: true }] },
    ],
    Stop: [
      { hooks: [{ type: 'command', command: hook('Stop'), timeout: 5 }] },
    ],
    SessionEnd: [
      { hooks: [{ type: 'command', command: hook('SessionEnd'), timeout: 3, async: true }] },
    ],
    FileChanged: [
      { hooks: [{ type: 'command', command: hook('FileChanged'), timeout: 3, async: true }] },
    ],
  },
};

writeFileSync(outFile, `${JSON.stringify(settings, null, 2)}\n`);
