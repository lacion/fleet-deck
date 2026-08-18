// Repository transport defaults are environment policy, not parser behavior.
// Coder's external-auth tokens are injected into HTTPS Git operations; SSH is
// a separate flow that requires the user to register Coder's generated public
// key with the forge. Prefer HTTPS on Coder so the shortest supported path is
// the default, while standalone FleetDeck keeps its historical SSH default.

export type RepoTransport = 'ssh' | 'https';

export function repoTransportChoice({
  setting = null,
  coder = false,
}: {
  setting?: string | null;
  coder?: boolean;
} = {}): {
  value: RepoTransport;
  source: 'override' | 'coder' | 'default';
} {
  if (setting === 'ssh' || setting === 'https') return { value: setting, source: 'override' };
  if (coder) return { value: 'https', source: 'coder' };
  return { value: 'ssh', source: 'default' };
}
