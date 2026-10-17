import { type ReactNode, useRef } from 'react';
import { trapTab, useFocusReturn } from '../app/focus';
import { useOverlay } from '../app/overlay';

type Props = {
  readonly title: string;
  readonly onClose: () => void;
  readonly children: ReactNode;
};

/** The modal frame of the integrations dialogs: Escape closes, Tab stays inside, focus returns. */
export function Dialog({ title, onClose, children }: Props) {
  const root = useRef<HTMLDivElement>(null);
  useFocusReturn(root);
  useOverlay();
  return (
    // The overlay click is a mouse shortcut; the Cancel button and Escape serve the keyboard.
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
        <h2>{title}</h2>
        {children}
      </div>
    </div>
  );
}
