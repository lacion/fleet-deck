// DL-179: the comment editor: a text box with "Add comment" (or another label) and "Cancel".
import { useEffect, useRef, useState } from 'react';

/** The most characters of a comment body. */
export const MAX_COMMENT_CHARS = 8000;

/** The text of an error that a call raised. */
export const failureText = (e: unknown): string =>
  e instanceof Error && e.message !== '' ? e.message : 'The request failed.';

export type CommentEditorProps = {
  readonly initial: string;
  /** The label of the primary button. */
  readonly submitLabel: string;
  /** A line above the box (the lines that the comment covers). */
  readonly title?: string | undefined;
  /** A line under the box. */
  readonly hint?: string | undefined;
  /** The box is not allowed to send (for example, a range that is too long). */
  readonly blocked?: string | undefined;
  /** Sends the body. A rejection keeps the editor open and shows its message. */
  readonly onSubmit: (body: string) => Promise<void>;
  /** Called after a successful send and on "Cancel". */
  readonly onClose: () => void;
  /** Called with the text on every change. */
  readonly onText?: ((text: string) => void) | undefined;
};

export function CommentEditor(props: CommentEditorProps) {
  const [text, setText] = useState(props.initial);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    const el = box.current;
    el?.focus();
    el?.setSelectionRange(el.value.length, el.value.length);
  }, []);
  const body = text.trim();
  const submit = () => {
    if (body === '' || saving || props.blocked !== undefined) return;
    setSaving(true);
    setError(null);
    props.onSubmit(body).then(
      () => props.onClose(),
      (e: unknown) => {
        setError(failureText(e));
        setSaving(false);
      },
    );
  };
  return (
    <div className="fd-cedit" data-editing="true">
      {props.title ? <p className="fd-cedit-title">{props.title}</p> : null}
      <textarea
        aria-label="Comment"
        className="fd-cedit-text"
        rows={4}
        maxLength={MAX_COMMENT_CHARS}
        value={text}
        ref={box}
        onChange={(e) => {
          setText(e.target.value);
          props.onText?.(e.target.value);
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
            e.preventDefault();
            submit();
          } else if (e.key === 'Escape') {
            e.preventDefault();
            props.onClose();
          }
        }}
      />
      {props.hint ? <p className="fd-cedit-hint">{props.hint}</p> : null}
      {props.blocked ? <p className="fd-cedit-hint">{props.blocked}</p> : null}
      {error !== null ? (
        <p role="alert" className="fd-cedit-alert">
          {error}
        </p>
      ) : null}
      <div className="fd-cedit-actions">
        <button
          type="button"
          className="fd-primary fd-sm"
          disabled={body === '' || saving || props.blocked !== undefined}
          onClick={submit}
        >
          {props.submitLabel}
        </button>
        <button type="button" className="fd-sm" onClick={props.onClose}>
          Cancel
        </button>
      </div>
    </div>
  );
}
