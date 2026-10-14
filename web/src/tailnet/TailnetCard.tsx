// DL-178: Settings → Deck → "Tailnet": shows what "Use tailnet" would save and saves it with an
// elevation (DL-172). The listeners start after a restart of the deck.
import { useCallback, useEffect, useRef, useState } from 'react';
import type { SettingsEnv } from '../settings/context';
import { useElevation } from '../settings/elevation';
import { DeckError, describe } from '../settings/http';
import type { TailnetView } from './api';
import { autoNotes, useButton, usesWindows } from './model';
import { saveDeckKey } from './save';

const list = (items: readonly string[]) => items.join(', ');

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
      <label>
        <input
          type="checkbox"
          checked={view.auto.enabled}
          disabled={busy}
          onChange={(e) => onAuto(e.target.checked)}
        />{' '}
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
}: {
  readonly view: TailnetView;
  readonly busy: boolean;
  readonly note: string | null;
  readonly onUse: () => void;
  readonly onAuto: (on: boolean) => void;
}) {
  return (
    <>
      {usesWindows(view) ? <p className="fd-note">{WINDOWS_TEXT}</p> : null}
      <CardBody view={view} busy={busy} onUse={onUse} />
      <AutoSection view={view} busy={busy} onAuto={onAuto} />
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

  // One deck key from the card: the automatic mode.
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
        />
      )}
      {error === null ? null : <p role="alert">{error}</p>}
      {node}
    </section>
  );
}
