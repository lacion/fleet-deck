import React from 'react';

// events/min over the last 30 minutes, as a tiny bar strip (mono data, faint).
export default function Sparkline({ data }) {
  const bars = Array.isArray(data) && data.length ? data.slice(-30) : [];
  // a single active minute renders as one lone bar (reads as noise) — wait
  // until there's an actual shape to show
  if (bars.filter((n) => n > 0).length < 2) return null;
  const max = Math.max(...bars, 1);
  const W = 60;
  const H = 12;
  const bw = W / 30;
  return (
    <svg className="fd-spark" width={W} height={H} aria-hidden="true">
      {bars.map((n, i) =>
        n > 0 ? (
          <rect
            key={i}
            x={i * bw}
            y={H - Math.max(2, (n / max) * H)}
            width={Math.max(1, bw - 0.7)}
            height={Math.max(2, (n / max) * H)}
            fill="var(--faint)"
            opacity={i === bars.length - 1 ? 1 : 0.75}
          />
        ) : null,
      )}
    </svg>
  );
}
