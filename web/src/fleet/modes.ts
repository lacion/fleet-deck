// The words of a mode ceiling of the fleet access forms (DESIGN-SYSTEM §5: the owner reads words,
// not protocol ids). A Claude mode has its word in ui/format (`acceptEdits` → "Accept edits"); a
// Codex mode is an approval policy and a sandbox (`on-request/workspace-write` → "Ask on request ·
// workspace write"). The option value stays the id.
import { modeLabel } from '../ui/format';

const APPROVAL: Readonly<Record<string, string>> = {
  untrusted: 'Untrusted',
  'on-request': 'Ask on request',
  never: 'Never ask',
};

const SANDBOX: Readonly<Record<string, string>> = {
  'read-only': 'read only',
  'workspace-write': 'workspace write',
  'danger-full-access': 'full access',
};

/** `on-request/workspace-write` → `Ask on request · workspace write`; an unknown id stays as it is. */
export function codexModeLabel(mode: string): string {
  const [approval, sandbox, ...rest] = mode.split('/');
  if (approval === undefined || sandbox === undefined || rest.length > 0) return mode;
  const a = APPROVAL[approval];
  const s = SANDBOX[sandbox];
  return a === undefined || s === undefined ? mode : `${a} · ${s}`;
}

/** The word of a Claude or a Codex mode ceiling. */
export const ceilingLabel = (mode: string): string =>
  mode.includes('/') ? codexModeLabel(mode) : modeLabel(mode);
