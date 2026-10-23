import { type ReactNode, useLayoutEffect, useRef } from 'react';
import { trapTab, useFocusReturn } from '../app/focus';
import { useOverlay } from '../app/overlay';
import { Icon, X } from '../ui/icons';

type Props = {
  readonly title: string;
  /** A line under the title in the head (§4.7, `--dim`): what the dialog asks. */
  readonly description?: ReactNode;
  readonly onClose: () => void;
  readonly children: ReactNode;
};

/** The controls that can take the focus (the list of `app/focus.ts`). */
const CONTROLS =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), summary, [tabindex]:not([tabindex="-1"])';

/**
 * The modal frame of the integrations dialogs: the dialog head (§4.7: the title, an optional
 * description, the Close X), Escape closes, Tab stays inside, focus returns. The first control of
 * the body takes the first focus (a form opens on its first field); the X is one Shift+Tab away,
 * and takes the focus only when the body has no control.
 */
export function Dialog({ title, description, onClose, children }: Props) {
  const root = useRef<HTMLDivElement>(null);
  const head = useRef<HTMLDivElement>(null);
  const start = useRef<HTMLElement | null>(null);
  // A layout effect runs before the focus effect of useFocusReturn.
  useLayoutEffect(() => {
    const controls = root.current?.querySelectorAll<HTMLElement>(CONTROLS) ?? [];
    start.current = [...controls].find((c) => !head.current?.contains(c)) ?? null;
  }, []);
  useFocusReturn(root, start);
  useOverlay();
  return (
    // The overlay click is a mouse shortcut; the Close X and Escape serve the keyboard.
    <div
      className="fd-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={root}
        className="fd-dialog fd-int-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            onClose();
          } else trapTab(e, root.current);
        }}
      >
        <div ref={head} className="fd-dialog-head">
          <h2>{title}</h2>
          {description === undefined ? null : <p>{description}</p>}
          <button
            type="button"
            className="fd-iconbtn"
            aria-label="Close"
            title="Close"
            onClick={onClose}
          >
            <Icon of={X} />
          </button>
        </div>
        {children}
      </div>
    </div>
  );
}
