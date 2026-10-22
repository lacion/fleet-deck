// DL-173: GitHub and GitLab. "Connect" starts the CLI device login on the station: the board shows
// the code and the page of the forge, and asks for the state every two seconds until it ends.
import { useEffect, useRef, useState } from 'react';
import type { AccountKind, AccountView, ForgeLogin } from '../gen';
import { Field } from '../ui/page';
import type { AccountActions } from './actions';
import { CopyButton } from './CopyButton';
import { endedText, KIND_LABEL, type LoginView, pollForgeLogin, startForgeLogin } from './flow';

const DEFAULT_HOST: Readonly<Partial<Record<AccountKind, string>>> = {
  github: 'github.com',
  gitlab: 'gitlab.com',
};

/** The link of the device page: only the address that the station answered, and only `https`. */
export const safeUrl = (url: string): string | null => (/^https:\/\//i.test(url) ? url : null);

export function DeviceLoginPanel({
  kind,
  login,
  state,
}: {
  readonly kind: AccountKind;
  readonly login: ForgeLogin;
  readonly state: LoginView;
}) {
  const href = safeUrl(login.url);
  const minutes = Math.max(1, Math.ceil(login.expires_in / 60));
  const problem = endedText(state);
  return (
    <div className="fd-set-device" role="group" aria-label={`${KIND_LABEL[kind]} device login`}>
      <p className="fd-note">
        Type this code on the page of {KIND_LABEL[kind]}. It works for {minutes}{' '}
        {minutes === 1 ? 'minute' : 'minutes'}.
      </p>
      <div className="fd-set-code">
        <code aria-label="Device code">{login.code}</code>
        <CopyButton text={login.code} label="Copy code" />
      </div>
      {href === null ? null : (
        <a href={href} target="_blank" rel="noopener noreferrer">
          Open {href}
        </a>
      )}
      {state.state === 'pending' ? (
        <p className="fd-note" role="status">
          Waiting for you to finish on that page…
        </p>
      ) : null}
      {state.state === 'done' ? (
        <p className="fd-set-ok" role="status">
          Connected as {state.user}.
        </p>
      ) : null}
      {problem === null ? null : (
        <p className="fd-fielderror" role="alert">
          {problem}
        </p>
      )}
    </div>
  );
}

export type Run =
  | { readonly phase: 'idle' }
  | { readonly phase: 'starting' }
  | { readonly phase: 'running'; readonly login: ForgeLogin; readonly state: LoginView }
  | { readonly phase: 'error'; readonly text: string; readonly token: boolean };

export function ForgeConnect({
  kind,
  station,
  actions,
  onConnected,
  onToken,
  initialRun,
  connected = false,
}: {
  readonly kind: 'github' | 'gitlab';
  readonly station: string;
  readonly actions: AccountActions;
  /**
   * The station has an account of this kind: the block folds under "Connect another <kind>
   * account…", so that a Connect button does not stand next to a connected account.
   */
  readonly connected?: boolean;
  /** The login ended: read the list again. */
  readonly onConnected: () => void;
  readonly onToken: (kind: AccountKind, host: string) => void;
  /** The login at the first render (tests). */
  readonly initialRun?: Run | undefined;
}) {
  const label = KIND_LABEL[kind];
  const [host, setHost] = useState(DEFAULT_HOST[kind] ?? '');
  const [run, setRun] = useState<Run>(initialRun ?? { phase: 'idle' });
  const [open, setOpen] = useState(false);
  const polling = useRef<AbortController | null>(null);
  useEffect(() => () => polling.current?.abort(), []);
  const login = run.phase === 'running' ? run.login : null;
  const loginId = login?.login_id;
  const pending = run.phase === 'running' && run.state.state === 'pending';
  useEffect(() => {
    if (loginId === undefined || !pending) return undefined;
    const controller = new AbortController();
    polling.current = controller;
    void pollForgeLogin(actions, station, loginId, {
      signal: controller.signal,
      wait: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
      onState: (state) => {
        setRun((cur) => (cur.phase === 'running' ? { ...cur, state } : cur));
        if (state.state === 'done') onConnected();
      },
    });
    return () => controller.abort();
  }, [actions, station, loginId, pending, onConnected]);
  const start = async () => {
    setRun({ phase: 'starting' });
    const out = await startForgeLogin(actions, station, kind, host);
    setRun(
      out.kind === 'started'
        ? { phase: 'running', login: out.login, state: { state: 'pending' } }
        : { phase: 'error', text: out.text, token: out.token },
    );
  };
  // A login that runs (or just ended) keeps the block open.
  const shown = !connected || open || run.phase !== 'idle';
  return (
    <div className="fd-set-connect" role="group" aria-label={`Connect ${label}`}>
      {connected ? (
        <button
          type="button"
          className="fd-ghost fd-sm"
          aria-expanded={shown}
          onClick={() => setOpen(!shown)}
        >
          Connect another {label} account…
        </button>
      ) : null}
      {shown ? (
        <ForgeForm
          label={label}
          host={host}
          placeholder={DEFAULT_HOST[kind]}
          run={run}
          onHost={setHost}
          onStart={() => void start()}
          onToken={() => onToken(kind, host.trim())}
          kind={kind}
        />
      ) : null}
    </div>
  );
}

/** The server field, the Connect button, and the state of a login. */
function ForgeForm({
  kind,
  label,
  host,
  placeholder,
  run,
  onHost,
  onStart,
  onToken,
}: {
  readonly kind: 'github' | 'gitlab';
  readonly label: string;
  readonly host: string;
  readonly placeholder: string | undefined;
  readonly run: Run;
  readonly onHost: (host: string) => void;
  readonly onStart: () => void;
  readonly onToken: () => void;
}) {
  return (
    <>
      <Field label="Server">
        <input
          value={host}
          autoComplete="off"
          spellCheck={false}
          placeholder={placeholder}
          onChange={(e) => onHost(e.target.value)}
        />
      </Field>
      <button
        type="button"
        disabled={run.phase === 'starting' || host.trim() === ''}
        onClick={onStart}
      >
        {run.phase === 'starting' ? 'Starting…' : `Connect ${label}`}
      </button>
      {run.phase === 'error' ? (
        <div className="fd-set-problem">
          <p className="fd-fielderror" role="alert">
            {run.text}
          </p>
          {run.token ? (
            <button type="button" className="fd-sm" onClick={onToken}>
              Use a token instead
            </button>
          ) : null}
        </div>
      ) : null}
      {run.phase === 'running' ? (
        <>
          <DeviceLoginPanel kind={kind} login={run.login} state={run.state} />
          {run.state.state === 'failed' || run.state.state === 'expired' ? (
            <button type="button" className="fd-sm" onClick={onToken}>
              Use a token instead
            </button>
          ) : null}
        </>
      ) : null}
    </>
  );
}

/** The forge accounts (GitHub and GitLab) of an account list. */
export const forgeAccounts = (accounts: readonly AccountView[]): AccountView[] =>
  accounts.filter((a) => a.kind === 'github' || a.kind === 'gitlab');
