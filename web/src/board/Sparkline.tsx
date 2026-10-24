const BAR = 2;
const GAP = 1;
const HEIGHT = 14;

/** The activity of the last 20 minutes as 20 bars; the last bar is the current minute. */
export function Sparkline({ values }: { readonly values: readonly number[] }) {
  const sum = values.reduce((total, n) => total + n, 0);
  const max = Math.max(...values, 0);
  const last = values.length - 1;
  return (
    <svg
      className="fd-spark"
      width={values.length * (BAR + GAP) - GAP}
      height={HEIGHT}
      role="img"
      aria-label={`Activity: ${sum} events in the last 20 minutes`}
    >
      {values.map((n, i) => {
        if (n <= 0) return null;
        const h = Math.max(1, (n / max) * HEIGHT);
        return (
          <rect
            key={i}
            x={i * (BAR + GAP)}
            y={HEIGHT - h}
            width={BAR}
            height={h}
            data-last={i === last ? 'true' : undefined}
          />
        );
      })}
    </svg>
  );
}
