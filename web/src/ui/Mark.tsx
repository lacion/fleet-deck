/** The board mark (DESIGN-SYSTEM.md §4.2): an ink rounded square with "FD" in the wide cut. */
export function Mark({ size = 24 }: { readonly size?: number }) {
  return (
    <svg
      className="fd-mark"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
    >
      <rect className="fd-mark-tile" width="24" height="24" rx="6" />
      <text
        className="fd-mark-text"
        x="12"
        y="12.5"
        fontSize="10"
        textAnchor="middle"
        dominantBaseline="central"
      >
        FD
      </text>
    </svg>
  );
}
