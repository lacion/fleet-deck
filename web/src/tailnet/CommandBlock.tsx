// The copy button and the command block of the Add station dialog.
import { useEffect, useRef, useState } from 'react';

const COPIED_MS = 1_500;

/** A button that copies `text`: "Copied" for 1.5 s, or a hint to select the text by hand. */
export function CopyButton({
  text,
  label = 'Copy',
}: {
  readonly text: string;
  readonly label?: string;
}) {
  const [copy, setCopy] = useState<'idle' | 'copied' | 'failed'>('idle');
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current);
    },
    [],
  );
  const run = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopy('copied');
      if (timer.current !== null) clearTimeout(timer.current);
      timer.current = setTimeout(() => setCopy('idle'), COPIED_MS);
    } catch {
      setCopy('failed');
    }
  };
  return (
    <>
      <button type="button" onClick={() => void run()}>
        {copy === 'copied' ? 'Copied' : label}
      </button>
      {copy === 'failed' ? <span role="alert">Copy failed: select the text</span> : null}
    </>
  );
}

/** A status line with a small spinner. */
export function WaitLine({ children }: { readonly children: string }) {
  return (
    <p className="fd-add-wait" role="status">
      <span className="fd-add-spinner" aria-hidden="true" />
      {children}
    </p>
  );
}

/** A command in a wrapping, selectable block with a Copy button. */
export function CommandBlock({ command }: { readonly command: string }) {
  return (
    <div className="fd-add-command">
      <code>{command}</code>
      <div className="fd-add-command-actions">
        <CopyButton text={command} />
      </div>
    </div>
  );
}
