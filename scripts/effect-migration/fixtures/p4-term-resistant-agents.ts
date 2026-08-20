import { writeFileSync } from 'node:fs';

const pidFile = process.argv[2];
if (!pidFile) throw new Error('P4 TERM-resistant agents fixture requires a pid-file path');

// The production agents poll launches this through the normal ProcessRunner
// facade. Ignore graceful TERM so a repeated daemon signal has to trip the P4
// force latch and synchronously SIGKILL the admitted process.
process.on('SIGTERM', () => undefined);
writeFileSync(pidFile, `${process.pid}\n`);
setInterval(() => undefined, 1_000);
await new Promise<never>(() => undefined);
