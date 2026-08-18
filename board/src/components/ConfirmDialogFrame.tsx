import { useRef, type ReactNode } from 'react';
import { useModal } from '../useModal.ts';

interface ConfirmDialogFrameProps {
  ariaLabel: string;
  busy: boolean;
  className?: string;
  title: ReactNode;
  titleClassName?: string;
  onCancel: () => void;
  children: ReactNode;
}

// Shared frame for the three compact confirmation dialogs. Their content and
// affirmative actions differ, but backdrop cancellation, busy-state Escape,
// focus trapping, header chrome, and click containment are one behavior.
export default function ConfirmDialogFrame({
  ariaLabel,
  busy,
  className = '',
  title,
  titleClassName = '',
  onCancel,
  children,
}: ConfirmDialogFrameProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  useModal(dialogRef, { initialFocus: false });

  const cancel = () => {
    if (!busy) onCancel();
  };

  return (
    <div className="fd-composewrap" onClick={cancel}>
      <div
        className={`fd-compose fd-killask${className ? ` ${className}` : ''}`}
        role="dialog"
        aria-modal="true"
        aria-label={ariaLabel}
        aria-busy={busy}
        ref={dialogRef}
        onKeyDown={(event) => {
          if (busy && event.key === 'Escape') {
            event.preventDefault();
            event.stopPropagation();
          }
        }}
        onClick={(event) => {
          event.stopPropagation();
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <span className={`lbl${titleClassName ? ` ${titleClassName}` : ''}`}>{title}</span>
          <span className="fd-spacer" />
          <button
            type="button"
            className="fd-x"
            aria-label="Cancel"
            disabled={busy}
            onClick={cancel}
          >
            ✕
          </button>
        </div>
        {children}
      </div>
    </div>
  );
}
