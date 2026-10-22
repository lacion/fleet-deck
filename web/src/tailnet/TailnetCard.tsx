// DL-178: Settings → Deck → "Tailnet": shows what "Use tailnet" would save and saves it with an
// elevation (DL-172). The listeners start after a restart of the deck.
import { useCallback, useEffect, useRef, useState } from 'react';
import type { SettingsEnv } from '../settings/context';
import { useElevation } from '../settings/elevation';
import { DeckError, describe } from '../settings/http';
import type { CertificateView, TailnetView } from './api';
import { autoNotes, useButton, usesWindows } from './model';
import { saveDeckKey } from './save';

const list = (items: readonly string[]) => items.join(', ');

const sameSet = (a: readonly string[], b: readonly string[]) => {
  const left = new Set(a);
  const right = new Set(b);
  return left.size === right.size && [...left].every((item) => right.has(item));
};

/** True when "Use tailnet" would change a listen address: the "With tailnet" row then shows. */
export const tailnetChanges = (view: TailnetView): boolean =>
  !sameSet(view.current.listen, view.managed.listen) ||
  !sameSet(view.current.link_listen, view.managed.link_listen);

/** The text of the card when the deck uses Tailscale for Windows from WSL (design 15.1). */
export const WINDOWS_TEXT =
  'Using Tailscale for Windows. It runs as your Windows user, with your Windows environment, the same as in a Windows terminal.';

/** The toggle "Use Tailscale automatically" with its help and its notes (design 14.4). */
function AutoSection({
  view,
  busy,
  onAuto,
}: {
  readonly view: TailnetView;
  readonly busy: boolean;
  readonly onAuto: (on: boolean) => void;
}) {
  const notes = autoNotes(view.auto);
  return (
    <div className="fd-tailnet-auto">
      <label className="fd-tailnet-switch">
        <input
          type="checkbox"
          role="switch"
          className="fd-switch"
          aria-checked={view.auto.enabled}
          checked={view.auto.enabled}
          disabled={busy}
          onChange={(e) => onAuto(e.target.checked)}
        />
        Use Tailscale automatically
      </label>
      <p className="fd-note">
        Keeps the tailnet addresses current at each start. Applies after a restart.
      </p>
      {notes.map((text) => (
        <p className="fd-note" key={text}>
          {text}
        </p>
      ))}
    </div>
  );
}

const FAILURES: Readonly<Record<string, string>> = {
  https_disabled:
    'HTTPS certificates are off for your tailnet. Turn them on in the Tailscale admin console → DNS → HTTPS Certificates.',
  not_logged_in: 'Tailscale is not logged in on this machine.',
  timeout: 'Tailscale did not answer in time.',
  not_installed: 'Tailscale was not found.',
  store_failed: 'The certificate could not be saved.',
};

/** The state line under the toggle "Trusted certificate from Tailscale"; `null` shows nothing. */
export function certificateLine(cert: CertificateView | undefined): string | null {
  switch (cert?.state) {
    case undefined:
    case 'off':
      return null;
    case 'renewing':
      return 'Getting the certificate…';
    case 'valid': {
      const until =
        cert.not_after === undefined
          ? ''
          : `, valid until ${new Date(cert.not_after).toLocaleDateString()}`;
      return `Trusted certificate for ${cert.name ?? 'this machine'}${until}`;
    }
    case 'expired':
      return 'The certificate expired. Fleet Deck is renewing it.';
    case 'failed':
      return FAILURES[cert.reason_code ?? ''] ?? 'Tailscale could not issue a certificate.';
    case 'name_mismatch':
      return "This machine's tailnet name differs from the board's. Use tailnet again, then restart the deck.";
  }
}

/** The toggle "Trusted certificate from Tailscale" with its help and its state (DL-190). */
function CertificateSection({
  cert,
  busy,
  onCert,
}: {
  readonly cert: CertificateView | undefined;
  readonly busy: boolean;
  readonly onCert: (on: boolean) => void;
}) {
  const line = certificateLine(cert);
  return (
    <div className="fd-tailnet-cert">
      <h3>Trusted certificate</h3>
      {cert?.owner_files === true ? (
        <p className="fd-note" data-note="owner-files">
          Your own certificate files (board.crt) are used for every name. Remove them to use the
          Tailscale certificate.
        </p>
      ) : null}
      <label className="fd-tailnet-switch">
        <input
          type="checkbox"
          role="switch"
          className="fd-switch"
          aria-checked={cert?.enabled ?? false}
          checked={cert?.enabled ?? false}
          disabled={busy}
          onChange={(e) => onCert(e.target.checked)}
        />
        Trusted certificate from Tailscale
      </label>
      <p className="fd-note">
        Browsers trust the board on its tailnet name. The name becomes public in Certificate
        Transparency logs.
      </p>
      {line === null ? null : <p role="status">{line}</p>}
    </div>
  );
}

function CardBody({
  view,
  busy,
  onUse,
}: {
  readonly view: TailnetView;
  readonly busy: boolean;
  readonly onUse: () => void;
}) {
  const button = useButton(view);
  const changes = tailnetChanges(view);
  if (!view.available) {
    return (
      <p className="fd-note">
        Tailscale is not available on the deck machine: {view.reason ?? 'no reason given'}. Pair
        stations with an invitation instead.
      </p>
    );
  }
  if (view.userspace) {
    return (
      <p className="fd-note">
        {view.reason ?? 'Tailscale runs in userspace networking: set the listen addresses by hand.'}
      </p>
    );
  }
  return (
    <>
      {view.self === undefined ? null : (
        <p>
          This deck on the tailnet: <code className="fd-tailnet-addr">{view.self.dns_name}</code> (
          <code className="fd-tailnet-addr">{list(view.self.ips)}</code>)
        </p>
      )}
      <table className="fd-tailnet-table">
        <thead>
          <tr>
            <th scope="col" />
            <th scope="col">Board address</th>
            <th scope="col">Link address</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <th scope="row">{changes ? 'Now' : 'Listens on'}</th>
            <td data-label="Board">{list(view.current.listen)}</td>
            <td data-label="Link">{list(view.current.link_listen)}</td>
          </tr>
          {changes ? (
            <tr>
              <th scope="row">With tailnet</th>
              <td data-label="Board">{list(view.managed.listen)}</td>
              <td data-label="Link">{list(view.managed.link_listen)}</td>
            </tr>
          ) : null}
        </tbody>
      </table>
      {button === 'none' ? (
        <p className="fd-note">The deck listens on its tailnet addresses.</p>
      ) : (
        <button type="button" disabled={busy} onClick={onUse}>
          {button === 'use' ? 'Use tailnet' : 'Update'}
        </button>
      )}
    </>
  );
}

/** The body of the card for one answer of the deck. It shows what it is given. */
export function TailnetCardView({
  view,
  busy,
  note,
  onUse,
  onAuto,
  onCert,
}: {
  readonly view: TailnetView;
  readonly busy: boolean;
  readonly note: string | null;
  readonly onUse: () => void;
  readonly onAuto: (on: boolean) => void;
  readonly onCert: (on: boolean) => void;
}) {
  return (
    <>
      {usesWindows(view) ? <p className="fd-note">{WINDOWS_TEXT}</p> : null}
      <CardBody view={view} busy={busy} onUse={onUse} />
      <AutoSection view={view} busy={busy} onAuto={onAuto} />
      <CertificateSection cert={view.certificate} busy={busy} onCert={onCert} />
      {note === null ? null : <p role="status">{note}</p>}
    </>
  );
}

export function TailnetCard({ env }: { readonly env: SettingsEnv }) {
  const { tailnet } = env;
  const [view, setView] = useState<TailnetView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const { ask, node } = useElevation(env.settings);
  const calls = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const load = useCallback(async () => {
    const call = ++calls.current;
    try {
      const next = await tailnet.view();
      if (!alive.current || call !== calls.current) return;
      setView(next);
      setError(null);
    } catch (e) {
      if (!alive.current || call !== calls.current) return;
      setError(describe(e));
    }
  }, [tailnet]);
  useEffect(() => {
    void load();
  }, [load]);

  const use = async () => {
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      const elevation = await ask(['Network: listen', 'Network: link_listen']);
      if (elevation === null) return;
      const answer = await tailnet.use(elevation);
      if (!alive.current) return;
      setNote(
        answer.restart_needed
          ? 'Saved. The new listeners start after a restart of the deck.'
          : 'Saved.',
      );
      await load();
    } catch (e) {
      // A refused elevation is dropped, so that the next try asks again.
      if (e instanceof DeckError && e.code === 'elevation_required') {
        env.settings.forgetElevation();
      }
      if (alive.current) setError(describe(e));
    } finally {
      if (alive.current) setBusy(false);
    }
  };

  // One deck key from the card: the automatic mode or the trusted certificate.
  const saveKey = async (
    key: string,
    value: unknown,
    labels: readonly string[],
    askFirst: boolean,
    done: string,
  ) => {
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      const out = await saveDeckKey(env.settings, ask, key, value, labels, askFirst);
      if (!alive.current) return;
      if (out.kind === 'failed') setError(out.text);
      if (out.kind === 'saved') {
        setNote(done);
        await load();
      }
    } finally {
      if (alive.current) setBusy(false);
    }
  };

  return (
    <section className="fd-panel fd-tailnet" aria-label="Tailnet">
      <h2>Tailnet</h2>
      {view === null && error === null ? <p className="fd-note">Checking Tailscale…</p> : null}
      {view === null ? null : (
        <TailnetCardView
          view={view}
          busy={busy}
          note={note}
          onUse={() => void use()}
          onAuto={(on) =>
            void saveKey(
              'tailnet_auto',
              on,
              ['Tailscale: use automatically'],
              on,
              on ? 'Saved. The tailnet addresses are set at the next start of the deck.' : 'Saved.',
            )
          }
          onCert={(on) =>
            void saveKey('tailscale_cert', on, ['Tailscale: trusted certificate'], on, 'Saved.')
          }
        />
      )}
      {error === null ? null : <p role="alert">{error}</p>}
      {node}
    </section>
  );
}
