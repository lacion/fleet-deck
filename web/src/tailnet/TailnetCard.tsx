// DL-178: Settings → Deck → "Tailnet": shows what "Use tailnet" would save and saves it with an
// elevation (DL-172). The listeners start after a restart of the deck.
import { useCallback, useEffect, useRef, useState } from 'react';
import type { SettingsEnv } from '../settings/context';
import { useElevation } from '../settings/elevation';
import { DeckError, describe } from '../settings/http';
import type { TailnetView } from './api';
import { useButton } from './model';

const list = (items: readonly string[]) => items.join(', ');

/** The body of the card for one answer of the deck. It shows what it is given. */
export function TailnetCardView({
  view,
  busy,
  note,
  onUse,
}: {
  readonly view: TailnetView;
  readonly busy: boolean;
  readonly note: string | null;
  readonly onUse: () => void;
}) {
  const button = useButton(view);
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
        Tailscale runs in userspace networking: set the listen addresses by hand.
      </p>
    );
  }
  return (
    <>
      {view.self === undefined ? null : (
        <p>
          This deck on the tailnet: {view.self.dns_name} ({list(view.self.ips)})
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
            <th scope="row">Now</th>
            <td>{list(view.current.listen)}</td>
            <td>{list(view.current.link_listen)}</td>
          </tr>
          <tr>
            <th scope="row">With tailnet</th>
            <td>{list(view.managed.listen)}</td>
            <td>{list(view.managed.link_listen)}</td>
          </tr>
        </tbody>
      </table>
      {button === 'none' ? (
        <p className="fd-note">The deck listens on its tailnet addresses.</p>
      ) : (
        <button type="button" disabled={busy} onClick={onUse}>
          {button === 'use' ? 'Use tailnet' : 'Update'}
        </button>
      )}
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

  return (
    <section className="fd-panel fd-tailnet" aria-label="Tailnet">
      <h2>Tailnet</h2>
      {view === null && error === null ? <p className="fd-note">Checking Tailscale…</p> : null}
      {view === null ? null : (
        <TailnetCardView view={view} busy={busy} note={note} onUse={() => void use()} />
      )}
      {error === null ? null : <p role="alert">{error}</p>}
      {node}
    </section>
  );
}
