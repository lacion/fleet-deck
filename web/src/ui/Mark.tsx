/** The board mark (ui-pass §3): a flight strip with its amber state edge. */
export function Mark({ size = 18 }: { readonly size?: number }) {
  return (
    <svg
      className="fd-mark"
      width={size}
      height={size}
      viewBox="0 0 18 18"
      aria-hidden="true"
      focusable="false"
    >
      <rect
        x="1.5"
        y="4"
        width="15"
        height="10"
        rx="2"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
      />
      <rect x="1.5" y="4" width="3.5" height="10" rx="1" fill="var(--act)" />
      <path
        d="M8 7.5h5.5M8 10.5h3.5"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  );
}
