import { type KeyboardEvent, type ReactNode, useRef, useState } from 'react';
import { useDismiss } from './dismiss';

/**
 * A `<details>` popover menu. Escape or a press outside it closes it; Escape returns the focus
 * to the summary. The body gets a `close` function.
 */
export function Menu(props: {
  readonly summary: ReactNode;
  readonly label?: string;
  readonly summaryClass?: string;
  readonly title?: string;
  readonly onClosed?: () => void;
  readonly children: (close: () => void) => ReactNode;
}) {
  const ref = useRef<HTMLDetailsElement>(null);
  const [open, setOpen] = useState(false);
  const close = () => {
    if (ref.current) ref.current.open = false;
  };
  useDismiss(ref, open, close);
  const onKeyDown = (event: KeyboardEvent<HTMLDetailsElement>) => {
    if (event.key !== 'Escape' || !ref.current?.open) return;
    event.stopPropagation();
    close();
    ref.current.querySelector('summary')?.focus();
  };
  return (
    <details
      className="fd-menu"
      ref={ref}
      onKeyDown={onKeyDown}
      onToggle={(event) => {
        setOpen(event.currentTarget.open);
        if (!event.currentTarget.open) props.onClosed?.();
      }}
    >
      <summary className={props.summaryClass} aria-label={props.label} title={props.title}>
        {props.summary}
      </summary>
      <div className="fd-menu-body">{props.children(close)}</div>
    </details>
  );
}
