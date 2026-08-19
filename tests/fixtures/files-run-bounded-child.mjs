#!/usr/bin/env node

import fs from 'node:fs';

const [mode, ...args] = process.argv.slice(2);

function countAt(index) {
  const value = Number(args[index]);
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`invalid byte count: ${args[index]}`);
  }
  return value;
}

switch (mode) {
  case 'bytes': {
    const stdoutBytes = countAt(0);
    const stderrBytes = countAt(1);
    const hold = args[2] === 'hold';
    fs.writeSync(1, Buffer.alloc(stdoutBytes, 0x61));
    setTimeout(() => {
      fs.writeSync(2, Buffer.alloc(stderrBytes, 0x62));
      if (hold) setInterval(() => {}, 1_000);
    }, 25);
    break;
  }
  case 'stdin': {
    const chunks = [];
    process.stdin.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
    process.stdin.on('end', () => {
      fs.writeSync(1, Buffer.concat(chunks).toString('hex'));
    });
    break;
  }
  case 'nonzero': {
    fs.writeSync(1, 'kept stdout');
    fs.writeSync(2, '  fatal: bounded fixture  \n');
    process.exitCode = 7;
    break;
  }
  case 'timeout': {
    const [pidFile, termMarker] = args;
    if (!pidFile || !termMarker) {
      throw new Error('timeout mode requires pid and TERM marker paths');
    }
    fs.writeFileSync(pidFile, String(process.pid));
    fs.writeSync(1, 'partial stdout');
    fs.writeSync(2, 'partial stderr');
    process.on('SIGTERM', () => {
      fs.writeFileSync(termMarker, 'TERM');
    });
    setInterval(() => {}, 1_000);
    break;
  }
  default:
    fs.writeSync(2, `unknown fixture mode: ${String(mode)}\n`);
    process.exitCode = 64;
}
