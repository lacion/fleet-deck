import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

export interface Distribution {
  count: number;
  min: number;
  p50: number;
  p95: number;
  p99: number;
  max: number;
  mean: number;
}

export interface CommandResult {
  command: string[];
  cwd: string;
  durationMs: number;
  exitCode: number;
  stdout: string;
  stderr: string;
}

function rounded(value: number): number {
  return Math.round(value * 1000) / 1000;
}

function percentile(sorted: readonly number[], fraction: number): number {
  if (sorted.length === 0) return 0;
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * fraction) - 1));
  return sorted[index] ?? 0;
}

export function summarize(values: readonly number[]): Distribution {
  const sorted = [...values].sort((a, b) => a - b);
  const total = sorted.reduce((sum, value) => sum + value, 0);
  return {
    count: sorted.length,
    min: rounded(sorted[0] ?? 0),
    p50: rounded(percentile(sorted, 0.5)),
    p95: rounded(percentile(sorted, 0.95)),
    p99: rounded(percentile(sorted, 0.99)),
    max: rounded(sorted.at(-1) ?? 0),
    mean: rounded(sorted.length === 0 ? 0 : total / sorted.length),
  };
}

export async function runCommand(
  command: readonly string[],
  options: {
    cwd?: string;
    env?: Record<string, string | undefined>;
    stdin?: Uint8Array;
    timeoutMs?: number;
  } = {},
): Promise<CommandResult> {
  const cwd = options.cwd ?? REPO_ROOT;
  const startedAt = performance.now();
  const child = Bun.spawn([...command], {
    cwd,
    env: options.env ?? process.env,
    stdin: options.stdin ?? null,
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const completion = Promise.all([
    Bun.readableStreamToText(child.stdout),
    Bun.readableStreamToText(child.stderr),
    child.exited,
  ]);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let completed: [string, string, number];
  try {
    completed =
      options.timeoutMs === undefined
        ? await completion
        : await Promise.race([
            completion,
            new Promise<never>((_resolve, reject) => {
              timer = setTimeout(
                () =>
                  reject(
                    new Error(
                      `${command.join(' ')} did not complete within ${options.timeoutMs}ms`,
                    ),
                  ),
                options.timeoutMs,
              );
            }),
          ]);
  } catch (error) {
    if (child.exitCode === null) child.kill('SIGKILL');
    let cleanupTimer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        completion,
        new Promise<void>((resolve) => {
          cleanupTimer = setTimeout(resolve, 1_000);
        }),
      ]);
    } finally {
      if (cleanupTimer) clearTimeout(cleanupTimer);
    }
    throw error;
  } finally {
    if (timer) clearTimeout(timer);
  }
  const [stdout, stderr, exitCode] = completed;
  return {
    command: [...command],
    cwd,
    durationMs: rounded(performance.now() - startedAt),
    exitCode,
    stdout,
    stderr,
  };
}

export function assertCommand(result: CommandResult): CommandResult {
  if (result.exitCode !== 0) {
    throw new Error(
      `${result.command.join(' ')} exited ${result.exitCode}\n${result.stderr || result.stdout}`,
    );
  }
  return result;
}

export function jsonLine(value: unknown): void {
  process.stdout.write(`${JSON.stringify(value, null, 2)}\n`);
}

export async function writeJsonReport(value: unknown): Promise<void> {
  const encoded = `${JSON.stringify(value, null, 2)}\n`;
  const inlineOutput = Bun.argv.find((arg) => arg.startsWith('--out='));
  const outputFlag = Bun.argv.indexOf('--out');
  if (inlineOutput && outputFlag !== -1) {
    throw new Error('pass --out only once');
  }
  const output =
    inlineOutput?.slice('--out='.length) ?? (outputFlag === -1 ? null : Bun.argv[outputFlag + 1]);
  if ((inlineOutput !== undefined || outputFlag !== -1) && !output) {
    throw new Error('--out requires a path');
  }
  if (output) await Bun.write(path.resolve(output), encoded);
  process.stdout.write(encoded);
}
