import { MARK_BOX, MARK_PATH } from './brand';

/** The Fleet Deck mark (DESIGN-SYSTEM.md §2.6): one path in the text color; the wordmark next to it names it. */
export function Mark({ size = 24 }: { readonly size?: number }) {
  return (
    <svg
      className="fd-mark"
      width={size}
      height={size}
      viewBox={`0 0 ${MARK_BOX} ${MARK_BOX}`}
      aria-hidden="true"
      focusable="false"
    >
      <path fillRule="evenodd" d={MARK_PATH} />
    </svg>
  );
}
