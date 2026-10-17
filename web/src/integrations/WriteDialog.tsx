// R3.9 (DL-164): every write to a system asks for an explicit confirm step ("Post this comment to
// <system> as you?"). The write then goes with `confirm: true` and a command id that this dialog
// made when the owner pressed "Continue" (a repeat of the same press writes nothing twice).
import { type ReactNode, useEffect, useRef, useState } from 'react';
import { commandId } from '../session/commands';
import { Icon, X } from '../ui/icons';
import type { WriteResult } from '../gen';
import { Dialog } from './Dialog';
import { webUrl } from './model';
import { message } from './useLoad';

type Step = 'edit' | 'confirm' | 'done';

type Props = {
  readonly title: string;
  /** The fields of the edit step. */
  readonly children: ReactNode;
  /** `null`: the fields are not ready, "Continue" stays off. */
  readonly question: string | null;
  /** What the write will send, shown in the confirm step. */
  readonly preview: ReactNode;
  readonly write: (id: string) => Promise<WriteResult>;
  readonly onClose: () => void;
  /** The step at the first render (tests). */
  readonly initialStep?: Step;
  readonly initialResult?: WriteResult | null;
};

/** The edit step, the confirm step and the result of one write. */
export function WriteDialog({
  title,
  children,
  question,
  preview,
  write,
  onClose,
  initialStep = 'edit',
  initialResult = null,
}: Props) {
  const [step, setStep] = useState<Step>(initialStep);
  const [id, setId] = useState(() => commandId());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<WriteResult | null>(initialResult);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const send = async () => {
    setBusy(true);
    setError(null);
    try {
      const done = await write(id);
      if (!alive.current) return;
      setResult(done);
      setStep('done');
    } catch (e) {
      if (alive.current) setError(message(e));
    } finally {
      if (alive.current) setBusy(false);
    }
  };
  return (
    <Dialog title={title} onClose={onClose}>
      {step === 'edit' ? (
        <form
          className="fd-int-form"
          onSubmit={(e) => {
            e.preventDefault();
            if (question === null) return;
            setId(commandId());
            setError(null);
            setStep('confirm');
          }}
        >
          {children}
          <div className="fd-pane-row">
            <button type="submit" className="fd-primary" disabled={question === null}>
              Continue
            </button>
            <button type="button" className="fd-ghost" onClick={onClose}>
              <Icon of={X} size={14} /> Cancel
            </button>
          </div>
        </form>
      ) : null}
      {step === 'confirm' ? (
        <div className="fd-int-form" data-step="confirm">
          <p className="fd-int-question">{question}</p>
          <div className="fd-int-preview">{preview}</div>
          {error === null ? null : (
            <p className="fd-note fd-int-error" role="alert">
              {error}
            </p>
          )}
          <div className="fd-pane-row">
            <button
              type="button"
              className="fd-primary"
              disabled={busy}
              onClick={() => void send()}
            >
              {busy ? 'Sending…' : 'Confirm'}
            </button>
            <button
              type="button"
              className="fd-ghost"
              disabled={busy}
              onClick={() => setStep('edit')}
            >
              Back
            </button>
          </div>
        </div>
      ) : null}
      {step === 'done' && result !== null ? (
        <div className="fd-int-form" role="status" data-step="done">
          <p>
            Done
            {result.actor === null ? '' : ` as ${result.actor}`}.{' '}
            {webUrl(result.url) === null ? null : (
              <a href={result.url} target="_blank" rel="noopener noreferrer">
                Open it
              </a>
            )}
          </p>
          <p className="fd-note">
            <code>{result.url}</code>
          </p>
          <div className="fd-pane-row">
            <button type="button" className="fd-primary" onClick={onClose}>
              Close
            </button>
          </div>
        </div>
      ) : null}
    </Dialog>
  );
}
