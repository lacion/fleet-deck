import { writeFileSync } from 'node:fs';

const pidFile = process.argv[2];
if (!pidFile) throw new Error('p4 acquisition deadline child requires a pid record path');

const descendant = Bun.spawn({
  cmd: [
    process.execPath,
    '--no-env-file',
    '-e',
    "process.on('SIGTERM',()=>{});setInterval(()=>{},60000)",
  ],
  stdin: 'ignore',
  stdout: 'ignore',
  stderr: 'ignore',
});

writeFileSync(pidFile, JSON.stringify({ pid: process.pid, descendantPid: descendant.pid }));

// Both processes deliberately ignore ordinary TERM. The root-owned Bun driver
// must force the complete inherited process group during acquisition abort.
process.on('SIGTERM', () => undefined);
setInterval(() => undefined, 60_000);
