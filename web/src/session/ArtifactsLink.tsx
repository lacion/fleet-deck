// FG-12 (design §15.6): the Artifacts link of a session. With a local folder it opens the file
// browser on the artifact root; with an address only it shows the address to open on a tailnet
// device; while the newest artifact event is `artifacts.revoked` it opens nothing.
import { useState } from 'react';
import type { ArtifactsDir, EventEnvelope } from '../gen';
import { Dialog } from '../integrations/Dialog';
import { FilesPanel } from './FilesPanel';
import type { FileActions } from './workspace';

/** True while the newest of `artifacts.ready` and `artifacts.revoked` is `artifacts.revoked`. */
export function artifactsRevoked(events: readonly EventEnvelope[]): boolean {
  for (let i = events.length - 1; i >= 0; i -= 1) {
    const kind = events[i]?.event.kind;
    if (kind === 'artifacts.revoked') return true;
    if (kind === 'artifacts.ready') return false;
  }
  return false;
}

/**
 * The machine of an artifact address `http://100.100.100.100:8080/<tailnet>/<machine>/<share>/...`:
 * the second path segment, or `null` when the address has no such segment.
 */
export function machineOfUrl(url: string): string | null {
  try {
    const segments = new URL(url).pathname.split('/').filter((s) => s !== '');
    const machine = segments[1];
    return machine === undefined ? null : decodeURIComponent(machine);
  } catch {
    return null;
  }
}

export function ArtifactsLink({
  dir,
  events,
  files,
}: {
  readonly dir: ArtifactsDir | undefined;
  readonly events: readonly EventEnvelope[];
  /** The file service of the artifact root; `null` without a reader or when the board is not live. */
  readonly files: FileActions | null;
}) {
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  if (artifactsRevoked(events)) {
    return (
      <p className="fd-note" role="status">
        The artifact share is not verified now.
      </p>
    );
  }
  if (dir === undefined) return null;
  if (dir.path !== undefined) {
    if (files === null) return null;
    return (
      <>
        <div className="fd-pane-row">
          <button type="button" className="fd-ghost fd-sm" onClick={() => setOpen(true)}>
            Artifacts
          </button>
        </div>
        {open ? (
          <Dialog title="Artifacts" onClose={() => setOpen(false)}>
            <FilesPanel files={files} root={dir.path} />
            <div className="fd-pane-row">
              <button type="button" className="fd-ghost" onClick={() => setOpen(false)}>
                Close
              </button>
            </div>
          </Dialog>
        ) : null}
      </>
    );
  }
  if (dir.url === undefined) return null;
  const url = dir.url;
  const machine = machineOfUrl(url);
  return (
    <div className="fd-pane-row">
      <p className="fd-note">
        The artifacts are on {machine ?? 'a tailnet machine'}. Open this address on a tailnet
        device: <code>{url}</code>
      </p>
      <button
        type="button"
        className="fd-ghost fd-sm"
        onClick={() => {
          void navigator.clipboard?.writeText(url).then(() => setCopied(true));
        }}
      >
        {copied ? 'Copied' : 'Copy address'}
      </button>
    </div>
  );
}
