// R13 P3c (design §11 P3): "Send to wall…" on a session and on an artifact. The owner picks a
// display (and Focus or Review for a session); the board sends a typed directive
// (`POST /api/displays/{id}/directive`, scope Drive). The directive is pinned by default, so the
// auto-focus of the wall does not take the screen back within seconds.
import { type FormEvent, type ReactNode, useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { type DisplayActions, NO_DISPLAYS_TEXT, noDisplays } from '../displays/actions';
import type { DirectiveBody, DisplayView } from '../gen';
import { Dialog } from '../integrations/Dialog';
import { useSettings } from '../settings/context';
import { describe } from '../settings/http';
import { Icon, Monitor } from '../ui/icons';

export type WallTarget =
  | { readonly kind: 'session'; readonly station_id: string; readonly session_id: string }
  | {
      readonly kind: 'artifact';
      readonly station_id: string;
      readonly artifact_id: string;
      readonly version: number;
      /** The version is one HTML file (the deck's rule for `app: true`). */
      readonly single_html?: boolean;
    };

/** The directive body for `target` (`filter` is none: the ceiling of the display stays). */
export function directiveFor(
  target: WallTarget,
  mode: 'focus' | 'review',
  pin: boolean,
  app = false,
): DirectiveBody {
  return target.kind === 'artifact'
    ? {
        mode: 'artifact',
        artifact: {
          station_id: target.station_id,
          artifact_id: target.artifact_id,
          version: target.version,
        },
        app,
        pin,
        filter: null,
      }
    : {
        mode,
        session: { station_id: target.station_id, session_id: target.session_id },
        pin,
        filter: null,
      };
}

/** "Run it as an app" shows for one HTML file and a display that allows apps. */
export const canRunAsApp = (target: WallTarget, display: DisplayView | undefined): boolean =>
  target.kind === 'artifact' && target.single_html === true && display?.allow_apps === true;

type Loaded =
  | { readonly kind: 'loading' }
  | { readonly kind: 'none' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'ready'; readonly displays: readonly DisplayView[] };

export function SendToWallDialog({
  target,
  actions,
  onClose,
}: {
  readonly target: WallTarget;
  readonly actions: DisplayActions;
  readonly onClose: () => void;
}) {
  const [loaded, setLoaded] = useState<Loaded>({ kind: 'loading' });
  const [chosen, setChosen] = useState('');
  const [mode, setMode] = useState<'focus' | 'review'>('focus');
  const [pin, setPin] = useState(true);
  const [asApp, setAsApp] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    actions.list().then(
      (list) => {
        if (!live) return;
        setLoaded({ kind: 'ready', displays: list.displays });
        setChosen(list.displays[0]?.display_id ?? '');
      },
      (e: unknown) =>
        live && setLoaded(noDisplays(e) ? { kind: 'none' } : { kind: 'error', text: describe(e) }),
    );
    return () => {
      live = false;
    };
  }, [actions]);
  const displays = loaded.kind === 'ready' ? loaded.displays : [];
  // The checkbox shows for one HTML file and a display that allows apps; a hidden one sends false.
  const showApp = canRunAsApp(
    target,
    displays.find((d) => d.display_id === chosen),
  );
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy || chosen === '') return;
    setBusy(true);
    setError(null);
    actions.directive(chosen, directiveFor(target, mode, pin, showApp && asApp)).then(
      () => {
        setBusy(false);
        setSent(`Sent to ${displays.find((d) => d.display_id === chosen)?.name ?? 'the display'}.`);
      },
      (e: unknown) => {
        setBusy(false);
        setError(describe(e));
      },
    );
  };
  return (
    <Dialog title="Send to wall" onClose={onClose}>
      <form className="fd-set-dialogform" onSubmit={submit} aria-label="Send to wall">
        {loaded.kind === 'loading' ? <p role="status">Loading…</p> : null}
        {loaded.kind === 'none' ? <p className="fd-note">{NO_DISPLAYS_TEXT}.</p> : null}
        {loaded.kind === 'error' ? (
          <p className="fd-fielderror" role="alert">
            {loaded.text}
          </p>
        ) : null}
        {loaded.kind === 'ready' && displays.length === 0 ? (
          <p className="fd-note">No wall display yet.</p>
        ) : null}
        {displays.length === 0 ? null : (
          <fieldset className="fd-disp-modes">
            <legend className="fd-fieldlabel">Display</legend>
            {displays.map((d) => (
              <label key={d.display_id} className="fd-disp-check">
                <input
                  type="radio"
                  name="send-display"
                  checked={chosen === d.display_id}
                  onChange={() => setChosen(d.display_id)}
                />{' '}
                {d.name}
              </label>
            ))}
          </fieldset>
        )}
        {target.kind === 'session' ? (
          <fieldset className="fd-disp-modes">
            <legend className="fd-fieldlabel">Show</legend>
            <label className="fd-disp-check">
              <input
                type="radio"
                name="send-mode"
                checked={mode === 'focus'}
                onChange={() => setMode('focus')}
              />{' '}
              Focus
            </label>
            <label className="fd-disp-check">
              <input
                type="radio"
                name="send-mode"
                checked={mode === 'review'}
                onChange={() => setMode('review')}
              />{' '}
              Review
            </label>
          </fieldset>
        ) : (
          <p className="fd-note">The wall shows version {target.version} of this artifact.</p>
        )}
        {showApp ? (
          <label className="fd-disp-check">
            <input type="checkbox" checked={asApp} onChange={(e) => setAsApp(e.target.checked)} />{' '}
            Run it as an app
          </label>
        ) : null}
        <label className="fd-disp-check">
          <input type="checkbox" checked={pin} onChange={(e) => setPin(e.target.checked)} /> Keep it
          on screen
        </label>
        {error === null ? null : (
          <p className="fd-fielderror" role="alert">
            {error}
          </p>
        )}
        {sent === null ? null : <p role="status">{sent}</p>}
        <div className="fd-formactions">
          {/* After a send the head X closes the dialog (§4.7: no text Close button). */}
          {sent === null ? (
            <button type="button" className="fd-ghost" onClick={onClose}>
              Cancel
            </button>
          ) : null}
          <button
            type="submit"
            className="fd-primary"
            disabled={busy || chosen === '' || displays.length === 0}
          >
            {busy ? 'Sending…' : 'Send'}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

/**
 * The control "Send to wall…" and its dialog. It renders nothing without the owner's settings
 * context (a guest, a non-admin login, a test without a deck). `children` is the button face.
 */
export function SendToWall({
  target,
  className,
  onOpen,
  children,
}: {
  readonly target: WallTarget;
  readonly className?: string;
  /** Called when the dialog opens (a menu closes itself). */
  readonly onOpen?: () => void;
  readonly children?: ReactNode;
}) {
  const env = useSettings();
  const [open, setOpen] = useState(false);
  if (env === undefined) return null;
  return (
    <>
      <button
        type="button"
        className={className}
        onClick={() => {
          onOpen?.();
          setOpen(true);
        }}
      >
        {children ?? (
          <>
            <Icon of={Monitor} /> Send to wall…
          </>
        )}
      </button>
      {open
        ? createPortal(
            <SendToWallDialog
              target={target}
              actions={env.displays}
              onClose={() => setOpen(false)}
            />,
            document.body,
          )
        : null}
    </>
  );
}
