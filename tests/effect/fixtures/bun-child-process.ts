const [, , mode, ...args] = process.argv;

switch (mode) {
  case 'inspect': {
    const input = await Bun.stdin.text();
    process.stdout.write(
      JSON.stringify({
        args,
        cwd: process.cwd(),
        input,
        liveEnv: process.env['FLEETDECK_EFFECT_CHILD_ENV'] ?? null,
      }),
    );
    process.stderr.write('fixture-stderr');
    break;
  }
  case 'emit': {
    process.stdout.write(args[0] ?? '');
    process.stderr.write(args[1] ?? '');
    break;
  }
  case 'upper': {
    process.stdout.write((await Bun.stdin.text()).toUpperCase());
    break;
  }
  case 'hold': {
    process.stdout.write('ready\n');
    setInterval(() => undefined, 1_000);
    break;
  }
  case 'stubborn': {
    process.on('SIGTERM', () => undefined);
    process.stdout.write('ready\n');
    setInterval(() => undefined, 1_000);
    break;
  }
  case 'signal-tree': {
    const descendant = Bun.spawn([process.execPath, import.meta.path, 'hold'], {
      stdin: 'ignore',
      stdout: 'ignore',
      stderr: 'ignore',
      detached: false,
    });
    process.stdout.write(`${String(descendant.pid)}\n`);
    setTimeout(() => process.kill(process.pid, 'SIGTERM'), 50);
    setInterval(() => undefined, 1_000);
    break;
  }
  case 'exit': {
    process.exitCode = Number.parseInt(args[0] ?? '0', 10);
    break;
  }
  default:
    process.stderr.write(`unknown fixture mode: ${String(mode)}\n`);
    process.exitCode = 64;
}
