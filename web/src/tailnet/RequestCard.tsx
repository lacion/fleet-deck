// One waiting request. The values on the card are the ones that the owner approves.
import type { PairRequest } from './api';
import { clockText, groupFingerprint, scopeText } from './model';

function identityText(request: PairRequest): string {
  const t = request.tailscale;
  if (t !== null && (t.host_name !== undefined || t.login !== undefined) && t.error === undefined) {
    const parts = [
      t.host_name,
      t.login,
      t.tags.length === 0 ? undefined : `tags ${t.tags.join(', ')}`,
    ].filter((part) => part !== undefined);
    return `Tailscale: ${parts.join(' · ')}`;
  }
  return 'Tailscale identity unavailable';
}

export function RequestCard({
  request,
  onApprove,
  onDeny,
  error,
  busy = false,
}: {
  readonly request: PairRequest;
  readonly onApprove: () => void;
  readonly onDeny: () => void;
  readonly error: string | null;
  readonly busy?: boolean;
}) {
  const name = request.requested_name;
  return (
    <article className="fd-pair-request" aria-label={`Pair request ${name}`}>
      <h4>{name} asks to join this deck</h4>
      {/* P8 §1: the exact instruction; the new machine shows the same 16 groups. */}
      <p>
        Compare all 16 groups with the fingerprint shown on the new machine. Approve only if every
        group matches.
      </p>
      <div className="fd-add-fingerprint">
        <code className="fd-fingerprint">{groupFingerprint(request.station_fingerprint)}</code>
      </div>
      <p>If you add it, this deck can:</p>
      <ul className="fd-add-scopes">
        {request.offered_scopes.map((scope) => (
          <li key={scope}>{scopeText(scope)}</li>
        ))}
      </ul>
      <p className="fd-note fd-add-small">
        From {request.source_ip} · {identityText(request)} · Ends at {clockText(request.expires_at)}
      </p>
      <div className="fd-pair-actions">
        <button type="button" className="fd-decide" disabled={busy} onClick={onApprove}>
          Yes, the codes match. Add {name}
        </button>
        <button type="button" className="fd-danger" disabled={busy} onClick={onDeny}>
          No, refuse
        </button>
      </div>
      {error === null ? null : <p role="alert">{error}</p>}
    </article>
  );
}
