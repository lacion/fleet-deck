// Shared tmux capability contract. Fleet Deck relies on `tmux -N` for probes
// that must never start a replacement server; -N was added in tmux 3.4.
export const MIN_TMUX_VERSION = '3.4';

export function parseTmuxVersion(output) {
  const match = /^tmux\s+(\d+)\.(\d+)([a-z][a-z0-9-]*)?\s*$/i.exec(String(output ?? ''));
  if (!match) return null;
  const major = Number(match[1]);
  const minor = Number(match[2]);
  if (!Number.isSafeInteger(major) || !Number.isSafeInteger(minor)) return null;
  return { major, minor, version: `${match[1]}.${match[2]}${match[3] ?? ''}` };
}

export function tmuxVersionSupported(output) {
  const parsed = parseTmuxVersion(output);
  return !!parsed && (parsed.major > 3 || (parsed.major === 3 && parsed.minor >= 4));
}

export function tmuxVersionCapability(output) {
  const parsed = parseTmuxVersion(output);
  if (!parsed) return { available: false, reason: `tmux version is unknown; tmux ${MIN_TMUX_VERSION}+ required` };
  if (!tmuxVersionSupported(output)) {
    return {
      available: false,
      version: parsed.version,
      reason: `tmux ${parsed.version} is too old; tmux ${MIN_TMUX_VERSION}+ required`,
    };
  }
  return { available: true, version: parsed.version };
}
