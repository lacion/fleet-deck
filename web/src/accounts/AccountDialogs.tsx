// DL-173: the dialogs of the Accounts tab: an API token, and the OAuth app of Jira or Linear. A
// secret input is not controlled: no React state holds the value. The parent reads it once when
// the form is sent, and the input is cleared at once.
import { type FormEvent, useRef, useState } from 'react';
import type { AccountKind } from '../gen';
import { Dialog } from '../integrations/Dialog';
import { useFocusWhen } from '../settings/dialogs';
import { Field } from '../ui/page';
import { CopyButton } from './CopyButton';
import { appSteps, KIND_LABEL, type SecretRead } from './flow';

function Problem({ text }: { readonly text: string | null }) {
  return text === null ? null : (
    <p className="fd-fielderror" role="alert">
      {text}
    </p>
  );
}

export type TokenForm = {
  readonly site: string;
  readonly email: string;
};

/** The label of the "site" input of a token: a Jira site URL, or a forge host. */
const SITE_LABEL: Readonly<Record<AccountKind, string>> = {
  jira: 'Jira site',
  linear: 'Site',
  github: 'Host',
  gitlab: 'Host',
};

export function TokenDialog({
  kind,
  initialSite,
  busy,
  error,
  onSubmit,
  onCancel,
}: {
  readonly kind: AccountKind;
  readonly initialSite: string;
  readonly busy: boolean;
  readonly error: string | null;
  readonly onSubmit: (form: TokenForm, secret: SecretRead) => void;
  readonly onCancel: () => void;
}) {
  const label = KIND_LABEL[kind];
  const [site, setSite] = useState(initialSite);
  const [email, setEmail] = useState('');
  const token = useRef<HTMLInputElement>(null);
  useFocusWhen(token, error);
  const clear = () => {
    if (token.current) token.current.value = '';
  };
  const close = () => {
    clear();
    onCancel();
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    onSubmit({ site, email }, { read: () => token.current?.value ?? '', clear });
  };
  return (
    <Dialog title={`Use a token for ${label}`} onClose={close}>
      <form className="fd-set-dialogform" onSubmit={submit} aria-label={`Use a token for ${label}`}>
        {kind === 'linear' ? null : (
          <Field
            label={SITE_LABEL[kind]}
            hint={kind === 'jira' ? 'For example https://acme.atlassian.net' : undefined}
          >
            <input
              value={site}
              required={kind === 'jira'}
              autoComplete="off"
              spellCheck={false}
              onChange={(e) => setSite(e.target.value)}
            />
          </Field>
        )}
        {kind === 'jira' ? (
          <Field label="Account email">
            <input
              type="email"
              value={email}
              required
              autoComplete="off"
              onChange={(e) => setEmail(e.target.value)}
            />
          </Field>
        ) : null}
        <Field label={kind === 'linear' ? 'Personal API key' : 'API token'}>
          <input ref={token} type="password" autoComplete="off" spellCheck={false} required />
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
            {busy ? 'Saving…' : 'Save token'}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

export function AppDialog({
  kind,
  redirectUrl,
  initialClientId,
  busy,
  error,
  onSubmit,
  onCancel,
}: {
  readonly kind: 'jira' | 'linear';
  readonly redirectUrl: string;
  readonly initialClientId: string;
  readonly busy: boolean;
  readonly error: string | null;
  readonly onSubmit: (clientId: string, secret: SecretRead) => void;
  readonly onCancel: () => void;
}) {
  const label = KIND_LABEL[kind];
  const [clientId, setClientId] = useState(initialClientId);
  const secret = useRef<HTMLInputElement>(null);
  useFocusWhen(secret, error);
  const clear = () => {
    if (secret.current) secret.current.value = '';
  };
  const close = () => {
    clear();
    onCancel();
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    onSubmit(clientId, { read: () => secret.current?.value ?? '', clear });
  };
  return (
    <Dialog title={`Set up the ${label} OAuth app`} onClose={close}>
      <form
        className="fd-set-dialogform"
        onSubmit={submit}
        aria-label={`Set up the ${label} OAuth app`}
      >
        <ol className="fd-set-steps">
          {appSteps(kind, redirectUrl).map((step) => (
            <li key={step}>{step}</li>
          ))}
        </ol>
        <div className="fd-set-redirect">
          <code>{redirectUrl}</code>
          <CopyButton text={redirectUrl} label="Copy" />
        </div>
        <Field label="Client id">
          <input
            value={clientId}
            required
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => setClientId(e.target.value)}
          />
        </Field>
        <Field label="Client secret">
          <input ref={secret} type="password" autoComplete="off" spellCheck={false} required />
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
            {busy ? 'Saving…' : 'Save app'}
          </button>
        </div>
      </form>
    </Dialog>
  );
}
