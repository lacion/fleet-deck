// Shared tmux capability contract. Fleet Deck relies on `tmux -N` for probes
// that must never start a replacement server; -N was added in tmux 3.4.
export const MIN_TMUX_VERSION = '3.4';

export interface TmuxVersion {
  major: number;
  minor: number;
  version: string;
}
export type TmuxCapability =
  { available: true; version: string } | { available: false; version?: string; reason: string };

export function parseTmuxVersion(output: unknown): TmuxVersion | null {
  // eslint-disable-next-line @typescript-eslint/no-base-to-string -- intentional String() coercion of untrusted command output, matching the pre-migration .mjs behavior
  const match = /^tmux\s+(\d+)\.(\d+)([a-z][a-z0-9-]*)?\s*$/i.exec(String(output ?? ''));
  if (!match) return null;
  const [, majorStr, minorStr, suffix] = match;
  // Groups 1 and 2 are non-optional in the regex, so they are always captured
  // when it matches; the guard only exists to narrow `string | undefined`
  // (noUncheckedIndexedAccess) — it never fires at runtime for a match.
  if (majorStr === undefined || minorStr === undefined) return null;
  const major = Number(majorStr);
  const minor = Number(minorStr);
  if (!Number.isSafeInteger(major) || !Number.isSafeInteger(minor)) return null;
  return { major, minor, version: `${majorStr}.${minorStr}${suffix ?? ''}` };
}

export function tmuxVersionSupported(output: unknown): boolean {
  const parsed = parseTmuxVersion(output);
  return !!parsed && (parsed.major > 3 || (parsed.major === 3 && parsed.minor >= 4));
}

export function tmuxVersionCapability(output: unknown): TmuxCapability {
  const parsed = parseTmuxVersion(output);
  if (!parsed)
    return {
      available: false,
      reason: `tmux version is unknown; tmux ${MIN_TMUX_VERSION}+ required`,
    };
  if (!tmuxVersionSupported(output)) {
    return {
      available: false,
      version: parsed.version,
      reason: `tmux ${parsed.version} is too old; tmux ${MIN_TMUX_VERSION}+ required`,
    };
  }
  return { available: true, version: parsed.version };
}
