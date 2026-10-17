// The copy button and the path shortener of the session header, moved out
// of `SessionHeader.tsx` for the module size rule.
import { useEffect, useState } from 'react';
import { Check, Copy, Icon } from '../ui/icons';

const COPIED_MS = 1500;

/** The longest path the header shows, in characters. */
export const MAX_PATH_CHARS = 48;

/** A long path loses its middle: `/home/lu…/repo/src`. */
export function shortenMiddle(text: string, max: number): string {
  const chars = [...text];
  if (chars.length <= max) return text;
  const keep = max - 1;
  const head = Math.ceil(keep / 2);
  const tail = keep - head;
  return `${chars.slice(0, head).join('')}…${chars.slice(chars.length - tail).join('')}`;
}

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
