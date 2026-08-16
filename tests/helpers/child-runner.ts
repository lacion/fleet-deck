// tests/helpers/child-runner.ts — launch a focused child test-run under whichever
// runtime is active.
//
// The cleanup-regression suites (spawn-repo-scratch-cleanup, spec-record-cleanup)
// prove a scratch-dir teardown by running a REAL, independent test process with
// TMPDIR pointed at a private sandbox, then asserting the sandbox is empty
// afterward. The child runner's CLI differs by runtime, and so does the shape of
// its passing-test count:
//
//   node: `node --test [--test-concurrency=1] --test-reporter=tap
//          [--test-name-pattern <pat>] <file>`   →  `# pass N` on STDOUT
//   bun:  `bun test [-t <pat>] <file>`            →  ` N pass`  on STDERR
//
// Because the two runtimes report the count on different streams, callers must
// feed the COMBINED stdout+stderr to childPassCount(). Under Node the argv this
// builds is byte-identical to what those suites hand-wrote before the seam, so
// the trust anchor is unchanged.

import process from 'node:process';

export interface ChildRunSpec {
  /** Absolute path to the .test.ts file to run. */
  file: string;
  /** Run only tests whose name matches (regex under both runtimes). */
  namePattern?: string;
  /** node: force `--test-concurrency=1`. bun: no-op — a file's tests run serially. */
  serial?: boolean;
}

/** Argv AFTER process.execPath for a focused child test run under the active runtime. */
export function childRunArgv({ file, namePattern, serial }: ChildRunSpec): string[] {
  if (process.versions.bun) {
    const argv = ['test'];
    if (namePattern) argv.push('-t', namePattern);
    argv.push(file);
    return argv;
  }
  const argv = ['--test'];
  if (serial) argv.push('--test-concurrency=1');
  argv.push('--test-reporter=tap');
  if (namePattern) argv.push('--test-name-pattern', namePattern);
  argv.push(file);
  return argv;
}

/**
 * Passing-test count parsed from a child run's COMBINED stdout+stderr. Node
 * prints `# pass N` (TAP, stdout); Bun prints ` N pass` (summary, stderr).
 * Returns 0 when no summary line is found (a silent all-skip or a crash).
 */
export function childPassCount(output: string): number {
  const re = process.versions.bun ? /^\s*(\d+)\s+pass\b/m : /^# pass (\d+)$/m;
  const m = re.exec(output);
  return m ? Number(m[1]) : 0;
}
