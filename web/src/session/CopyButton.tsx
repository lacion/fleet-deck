// The copy button of the session header, moved out of `SessionHeader.tsx` for the module size
// rule. The home folder shortener is `homeShort` of ui/format.
import { useEffect, useState } from 'react';
import { Check, Copy, Icon } from '../ui/icons';

const COPIED_MS = 1500;

/** An icon button that copies `text`; it shows a check for 1.5 s. */
export function CopyButton({ text, label }: { readonly text: string; readonly label: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), COPIED_MS);
    return () => clearTimeout(timer);
  }, [copied]);
  return (
    <button
      type="button"
      className="fd-iconbtn fd-sm"
      aria-label={label}
      title={label}
      onClick={() => {
        navigator.clipboard
          .writeText(text)
          .then(() => setCopied(true))
          .catch(() => setCopied(false));
      }}
    >
      <Icon of={copied ? Check : Copy} size={14} />
    </button>
  );
}
