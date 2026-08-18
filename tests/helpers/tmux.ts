import { spawnSync } from 'node:child_process';
import { lstatSync, unlinkSync } from 'node:fs';
import path from 'node:path';

// Test-owned labels only. Keep this deliberately narrower than `fleetdeck-*`:
// cleanup must never unlink a developer or production tmux socket merely
// because its name shares the product prefix.
const OWNED_SOCKET_RE =
  /^fleetdeck-(?:test-\d+|adapter(?:-(?:probe|duplicate|recycle|expect|trap))?-\d+-[0-9a-f]{8})$/;

const ABSENT_RE =
  /(?:no server running on|error connecting to .*\((?:connection refused|no such file or directory)\))/i;

function commandEnv(tmuxTmpDir: string | undefined): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env };
  if (tmuxTmpDir === undefined) delete env['TMUX_TMPDIR'];
  else env['TMUX_TMPDIR'] = tmuxTmpDir;
  // A client inherited from an outer tmux must still address the explicit -L
  // test socket, never the developer's current server.
  delete env['TMUX'];
  delete env['TMUX_PANE'];
  return env;
}

/** Resolve the exact Unix socket path tmux uses for a test-owned `-L` label. */
export function ownedTmuxSocketPath(
  socket: string | undefined,
  tmuxTmpDir: string | undefined,
): string | null {
  if (!socket || !OWNED_SOCKET_RE.test(socket)) return null;
  const uid = process.getuid?.();
  if (uid === undefined) return null;
  const root = tmuxTmpDir?.trim() ? path.resolve(tmuxTmpDir) : '/tmp';
  return path.join(root, `tmux-${uid}`, socket);
}

/**
 * Stop one exact FleetDeck test tmux server, then unlink only a verified stale
 * socket inode. A live response, an unavailable tmux binary, an unowned label,
 * or a non-socket filesystem entry all fail closed.
 */
export function cleanupOwnedTmuxSocket(
  socket: string | undefined,
  tmuxTmpDir: string | undefined,
): boolean {
  const socketPath = ownedTmuxSocketPath(socket, tmuxTmpDir);
  if (!socketPath || !socket) return false;

  const env = commandEnv(tmuxTmpDir);
  const options = { encoding: 'utf8' as const, timeout: 3_000, env };
  const killed = spawnSync('tmux', ['-L', socket, 'kill-server'], options);
  const probe = spawnSync('tmux', ['-L', socket, 'list-sessions'], options);
  if (probe.status === 0 || probe.error) return false;

  const probeText = `${probe.stdout ?? ''}\n${probe.stderr ?? ''}`;
  if (killed.status !== 0 && !(probe.status === 1 && ABSENT_RE.test(probeText))) return false;

  try {
    const entry = lstatSync(socketPath);
    if (!entry.isSocket()) return false;
    unlinkSync(socketPath);
    return true;
  } catch {
    // Already absent is the common successful kill-server outcome on Linux.
    return false;
  }
}
