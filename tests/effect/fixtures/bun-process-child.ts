import { writeSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

function requiredArgument(arguments_: readonly string[], index: number, name: string): string {
  const value = arguments_[index];
  if (value === undefined) throw new Error(`missing ${name}`);
  return value;
}

function positiveInteger(value: string, name: string): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 0) throw new Error(`invalid ${name}: ${value}`);
  return parsed;
}

async function readStdin(): Promise<string> {
  process.stdin.setEncoding('utf8');
  let input = '';
  for await (const chunk of process.stdin) input += String(chunk);
  return input;
}

const mode = requiredArgument(process.argv, 2, 'mode');
const arguments_ = process.argv.slice(3);

switch (mode) {
  case 'roundtrip': {
    const literal = requiredArgument(arguments_, 0, 'literal argv');
    const stdin = await readStdin();
    writeSync(2, 'roundtrip stderr is drained');
    writeSync(
      1,
      JSON.stringify({
        literal,
        cwd: process.cwd(),
        liveEnv: process.env['FLEETDECK_BUN_DRIVER_LIVE_ENV'],
        overrideEnv: process.env['FLEETDECK_BUN_DRIVER_OVERRIDE_ENV'],
        stdin,
      }),
    );
    break;
  }
  case 'nonzero': {
    writeSync(1, 'stdout is not the diagnostic');
    writeSync(2, '  fatal: bun fixture failed  \n');
    process.exit(7);
    break;
  }
  case 'bytes': {
    const stdoutBytes = positiveInteger(
      requiredArgument(arguments_, 0, 'stdout bytes'),
      'stdout bytes',
    );
    const stderrBytes = positiveInteger(
      requiredArgument(arguments_, 1, 'stderr bytes'),
      'stderr bytes',
    );
    writeSync(1, Buffer.alloc(stdoutBytes, 0x61));
    writeSync(2, Buffer.alloc(stderrBytes, 0x62));
    break;
  }
  case 'fragmented-utf8': {
    const asciiBytes = positiveInteger(
      requiredArgument(arguments_, 0, 'ascii bytes'),
      'ascii bytes',
    );
    writeSync(1, Buffer.alloc(asciiBytes, 0x61));
    writeSync(1, Buffer.from([0xf0]));
    await new Promise((resolve) => setTimeout(resolve, 10));
    writeSync(1, Buffer.from([0x9f, 0x92, 0xa9]));
    break;
  }
  case 'incomplete-utf8': {
    const asciiBytes = positiveInteger(
      requiredArgument(arguments_, 0, 'ascii bytes'),
      'ascii bytes',
    );
    writeSync(1, Buffer.alloc(asciiBytes, 0x61));
    writeSync(1, Buffer.from([0xc3]));
    break;
  }
  case 'stdin-early-exit': {
    // Deliberately never touch process.stdin. The parent queues far more than a
    // pipe can accept, while this process publishes one deterministic result
    // and closes before that finite input can be consumed.
    writeSync(1, 'stdin was not consumed\n');
    process.exit(0);
    break;
  }
  case 'term-resistant': {
    const pidFile = requiredArgument(arguments_, 0, 'pid file');
    process.on('SIGTERM', () => undefined);
    await Bun.write(pidFile, String(process.pid));
    setInterval(() => undefined, 1_000);
    await new Promise<never>(() => undefined);
    break;
  }
  case 'group-parent': {
    const helperPidFile = requiredArgument(arguments_, 0, 'helper pid file');
    const fixture = fileURLToPath(import.meta.url);
    Bun.spawn([process.execPath, fixture, 'term-resistant', helperPidFile], {
      stdin: 'ignore',
      stdout: 'inherit',
      stderr: 'inherit',
    });
    setInterval(() => undefined, 1_000);
    await new Promise<never>(() => undefined);
    break;
  }
  case 'term-resistant-group-parent': {
    const helperPidFile = requiredArgument(arguments_, 0, 'helper pid file');
    const fixture = fileURLToPath(import.meta.url);
    process.on('SIGTERM', () => undefined);
    Bun.spawn([process.execPath, fixture, 'term-resistant', helperPidFile], {
      stdin: 'ignore',
      stdout: 'inherit',
      stderr: 'inherit',
    });
    setInterval(() => undefined, 1_000);
    await new Promise<never>(() => undefined);
    break;
  }
  case 'immediate':
    break;
  default:
    throw new Error(`unknown bun process fixture mode: ${mode}`);
}
