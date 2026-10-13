import { useEffect, useRef, useState } from 'react';
import { Check, Copy, Icon } from '../ui/icons';

const COPIED_MS = 2000;

/** "Copy": puts `text` on the clipboard and says so for two seconds. */
export function CopyButton({
  text,
  label,
  className,
}: {
  readonly text: string;
  readonly label: string;
  readonly className?: string;
}) {
  const [state, setState] = useState<'idle' | 'done' | 'failed'>('idle');
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(
    () => () => {
      if (timer.current !== undefined) clearTimeout(timer.current);
    },
    [],
  );
  const copy = async () => {
    if (timer.current !== undefined) clearTimeout(timer.current);
    try {
      await navigator.clipboard.writeText(text);
      setState('done');
      timer.current = setTimeout(() => setState('idle'), COPIED_MS);
    } catch {
      setState('failed');
    }
  };
  return (
    <>
      <button type="button" className={className ?? 'fd-sm'} onClick={() => void copy()}>
        <Icon of={state === 'done' ? Check : Copy} size={14} />{' '}
        {state === 'done' ? 'Copied' : label}
      </button>
      {state === 'failed' ? (
        <span className="fd-fielderror" role="alert">
          The browser did not allow it. Select the text and copy it.
        </span>
      ) : null}
    </>
  );
}
