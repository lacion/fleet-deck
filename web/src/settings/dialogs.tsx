// DL-172: the dialogs of the settings: the elevation ("Confirm it is you"), the secret value, and
// the restart. They show what they are given; the page holds the state and calls the deck.
import { type FormEvent, type ReactNode, type RefObject, useEffect, useRef } from 'react';
import type { RestartItem } from '../gen';
import { useOverlay } from '../app/overlay';
import { Dialog } from '../integrations/Dialog';
import { Field } from '../ui/page';
import type { Credentials } from './actions';

/** Puts the focus on `target` when `when` changes to something: a disabled button drops the focus. */
export function useFocusWhen(target: RefObject<HTMLElement | null>, when: unknown): void {
  useEffect(() => {
    if (when !== null && when !== false && when !== undefined) target.current?.focus();
  }, [target, when]);
}

/** The error line of a dialog. */
function Problem({ text }: { readonly text: string | null }) {
  return text === null ? null : (
    <p className="fd-fielderror" role="alert">
      {text}
    </p>
  );
}

// --- the elevation ---

/**
 * "Confirm it is you": the owner's password, or the code of `fleetdeck open --elevate`. Both
 * inputs are cleared when the form is sent.
 */
export function ElevationDialog({
  labels,
  busy,
  error,
  onSubmit,
  onCancel,
}: {
  /** The labels of the changes that need the confirmation, when they are known. */
  readonly labels: readonly string[];
  readonly busy: boolean;
  readonly error: string | null;
  readonly onSubmit: (credentials: Credentials) => void;
  readonly onCancel: () => void;
}) {
  const password = useRef<HTMLInputElement>(null);
  const code = useRef<HTMLInputElement>(null);
  useFocusWhen(password, error);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    const p = password.current?.value ?? '';
    const c = (code.current?.value ?? '').trim();
    if (password.current) password.current.value = '';
    if (code.current) code.current.value = '';
    if (p !== '') onSubmit({ password: p });
    else if (c !== '') onSubmit({ code: c });
  };
  return (
    <Dialog title="Confirm it is you" onClose={onCancel}>
      <form className="fd-set-dialogform" onSubmit={submit} aria-label="Confirm it is you">
        {labels.length === 0 ? null : (
          <p className="fd-note">This change needs it: {labels.join(', ')}.</p>
        )}
        <Field label="Password">
          <input
            ref={password}
            type="password"
            autoComplete="current-password"
            spellCheck={false}
          />
        </Field>
        <p className="fd-note">
          No password? Run <code>fleetdeck open --elevate</code> on the deck machine and type the
          code.
        </p>
        <Field label="Code">
          <input ref={code} autoComplete="off" spellCheck={false} />
        </Field>
        <Problem text={error} />
        <div className="fd-formactions">
          <button type="button" className="fd-ghost" onClick={onCancel}>
            Cancel
          </button>
          <button type="submit" className="fd-primary" disabled={busy}>
            {busy ? 'Checking…' : 'Confirm'}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

// --- a secret value ---

/** The env variable names that a provider login accepts (the station checks them again). */
export const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** What the secret dialog gives its parent: the name, and the way to read and clear the input. */
export type SecretAccess = {
  readonly read: () => string;
  readonly clear: () => void;
};

/**
 * The value of a secret. The input is not controlled: no React state holds the value. The parent
 * reads it once, with `access`, when the form is sent, and the input is cleared at once.
 */
export function SecretDialog({
  title,
  name,
  busy,
  error,
  onSubmit,
  onCancel,
}: {
  readonly title: string;
  /** The fixed name of the variable ("Replace"); `null`: the owner types the name. */
  readonly name: string | null;
  readonly busy: boolean;
  readonly error: string | null;
  readonly onSubmit: (name: string, access: SecretAccess) => void;
  readonly onCancel: () => void;
}) {
  const nameInput = useRef<HTMLInputElement>(null);
  const value = useRef<HTMLInputElement>(null);
  useFocusWhen(value, error);
  const clear = () => {
    if (value.current) value.current.value = '';
  };
  const close = () => {
    clear();
    onCancel();
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    const typed = name ?? (nameInput.current?.value ?? '').trim();
    if (!ENV_NAME.test(typed)) return;
    onSubmit(typed, { read: () => value.current?.value ?? '', clear });
  };
  return (
    <Dialog title={title} onClose={close}>
      <form className="fd-set-dialogform" onSubmit={submit} aria-label={title}>
        {name === null ? (
          <Field label="Variable name" hint="For example ANTHROPIC_API_KEY.">
            <input
              ref={nameInput}
              autoComplete="off"
              spellCheck={false}
              pattern="[A-Za-z_][A-Za-z0-9_]*"
              required
            />
          </Field>
        ) : (
          <p className="fd-note">
            Variable <code>{name}</code>
          </p>
        )}
        <Field label="Value">
          <input ref={value} type="password" autoComplete="off" spellCheck={false} required />
        </Field>
        <p className="fd-note">
          Fleet Deck sends this once to the station and never shows it again.
        </p>
        <Problem text={error} />
        <div className="fd-formactions">
          <button type="button" className="fd-ghost" onClick={close}>
            Cancel
          </button>
          <button type="submit" className="fd-primary" disabled={busy}>
            {busy ? 'Saving…' : 'Save'}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

// --- the restart ---

export type RestartView =
  | { readonly phase: 'confirm' }
  | { readonly phase: 'busy' }
  | { readonly phase: 'would_stop'; readonly items: readonly RestartItem[] }
  | { readonly phase: 'failed'; readonly text: string };

const KIND_TEXT: Readonly<Record<string, string>> = {
  session: 'Session',
  shell: 'Terminal',
  setup: 'Setup command',
};

export function RestartDialog({
  name,
  view,
  onConfirm,
  onCancel,
}: {
  readonly name: string;
  readonly view: RestartView;
  /** `true`: stop the live sessions that the restart ends. */
  readonly onConfirm: (stopDirectSessions: boolean) => void;
  readonly onCancel: () => void;
}) {
  const busy = view.phase === 'busy';
  const action = useRef<HTMLButtonElement>(null);
  // The button that was pressed is disabled while the call runs: the focus returns when it ends.
  useFocusWhen(action, view.phase === 'busy' ? null : view.phase);
  let body: ReactNode;
  let actionButton: ReactNode;
  if (view.phase === 'would_stop') {
    const n = view.items.length;
    body = (
      <>
        <p>A restart ends this work:</p>
        <ul className="fd-set-stoplist">
          {view.items.map((item) => (
            <li key={`${item.kind}/${item.id}`}>
              <span className="fd-badge">{KIND_TEXT[item.kind] ?? item.kind}</span> {item.title}
            </li>
          ))}
        </ul>
      </>
    );
    actionButton = (
      <button ref={action} type="button" className="fd-danger" onClick={() => onConfirm(true)}>
        Restart and stop {n}
      </button>
    );
  } else {
    body = (
      <>
        <p>
          {name} stops and starts again. Open terminals and setup commands end. Sessions that keep
          running outside the process stay.
        </p>
        <Problem text={view.phase === 'failed' ? view.text : null} />
      </>
    );
    actionButton = (
      <button
        ref={action}
        type="button"
        className="fd-primary"
        disabled={busy}
        onClick={() => onConfirm(false)}
      >
        {busy ? 'Restarting…' : 'Restart'}
      </button>
    );
  }
  return (
    <Dialog title={`Restart ${name}`} onClose={onCancel}>
      <div className="fd-set-dialogform">
        {body}
        <div className="fd-formactions">
          <button type="button" className="fd-ghost" onClick={onCancel}>
            Cancel
          </button>
          {actionButton}
        </div>
      </div>
    </Dialog>
  );
}

/** How long the board waits before it takes a quick restart as done when it never saw a drop. */
export const RESTART_GRACE_MS = 10_000;
/** How long the board waits for the connection before it tells the owner to look. */
export const RESTART_GIVE_UP_MS = 90_000;

/**
 * The next state of the wait after a restart. The board is done when the connection is live again
 * after a drop; a restart too quick to see ends after the grace time.
 */
export function waitStep(
  sawDown: boolean,
  live: boolean,
  elapsedMs: number,
): { readonly sawDown: boolean; readonly done: boolean; readonly giveUp: boolean } {
  const down = sawDown || !live;
  const done = live && (down || elapsedMs >= RESTART_GRACE_MS);
  return { sawDown: down, done, giveUp: !done && elapsedMs >= RESTART_GIVE_UP_MS };
}

/** The full-page overlay while a process restarts. */
export function RestartOverlay({
  name,
  giveUp,
  onClose,
}: {
  readonly name: string;
  readonly giveUp: boolean;
  readonly onClose: () => void;
}) {
  useOverlay();
  return (
    <div className="fd-overlay fd-set-restart" role="alert" aria-live="assertive">
      <div className="fd-dialog">
        <h2>Restarting…</h2>
        <p>
          {giveUp
            ? `The board has not seen ${name} come back. Check that it runs, then close this.`
            : 'The board reconnects by itself.'}
        </p>
        {giveUp ? (
          <button type="button" onClick={onClose}>
            Close
          </button>
        ) : (
          <span className="fd-set-spinner" aria-hidden="true" />
        )}
      </div>
    </div>
  );
}
