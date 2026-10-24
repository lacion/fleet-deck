// B29: `.fd-bars`, a bar chart of one series (design analytics.md §3.11, DESIGN-SYSTEM §3). One
// inline SVG: a bar is a focusable `<rect>` with its value in `aria-label`; the readout line above
// the chart shows the bar under the pointer or the focus, else the total; a hidden table repeats
// the values. The labels are HTML (the SVG stretches, text in it would stretch too).
import { useState } from 'react';
import { monthDay } from './learnings';

export type Bar = {
  /** The text of the bar: `Oct 3`, or `Oct 3, 14:00` for an hour. */
  readonly label: string;
  readonly value: number;
};

type Props = {
  /** The words of the metric: `Cost`, `Tokens`. */
  readonly metric: string;
  readonly bars: readonly Bar[];
  /** The text of one value (`$1.20`, `9.4K`). */
  readonly format: (value: number) => string;
  /** The value of the readout while no bar has the focus (the total of the window). */
  readonly total: number;
  /** The first bar that has data; the bars before it are not drawn (no zero is invented). */
  readonly dataFrom?: number;
  /** The ms of the first transcript record: the text `No data before <Mon D>`. */
  readonly dataSince?: number | null;
  /** `neutral`: a series that is not usage (`--dim`). */
  readonly tone?: 'neutral';
  /** The unit of a bar: the group name is `<metric> by day` (an hour for the past 24 hours). */
  readonly by?: 'day' | 'hour';
};

/** The viewBox: the width is in units, the SVG stretches to the column. */
const WIDTH = 1000;
const HEIGHT = 160;
/** The room above the tallest bar for the top grid line. */
const TOP = 4;

/** The rounded maximum of the scale: 1, 2 or 5 times a power of ten that holds `max`. */
export function niceMax(max: number): number {
  if (!(max > 0)) return 1;
  const power = 10 ** Math.floor(Math.log10(max));
  for (const step of [1, 2, 5, 10]) {
    if (step * power >= max) return step * power;
  }
  return 10 * power;
}

export function Bars({
  metric,
  bars,
  format,
  total,
  dataFrom = 0,
  dataSince = null,
  tone,
  by = 'day',
}: Props) {
  const [active, setActive] = useState<number | null>(null);
  const count = bars.length;
  if (count === 0) return null;
  const top = niceMax(Math.max(0, ...bars.map((b) => b.value)));
  const step = WIDTH / count;
  const gap = count > 90 ? 1 : 2;
  const height = HEIGHT - TOP;
  const shown = active === null ? undefined : bars[active];
  const middle = bars[Math.floor((count - 1) / 2)];
  const first = bars[Math.min(dataFrom, count - 1)];
  const last = bars[count - 1];
  return (
    <div className="fd-bars" data-tone={tone}>
      <p className="fd-bars-readout">
        {shown === undefined
          ? `Total · ${format(total)}`
          : `${shown.label} · ${format(shown.value)}`}
      </p>
      <div className="fd-bars-plot">
        <span className="fd-bars-top">{format(top)}</span>
        <svg
          className="fd-bars-svg"
          viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
          preserveAspectRatio="none"
          role="group"
          aria-label={`${metric} by ${by}`}
        >
          <line className="fd-bars-grid" x1="0" x2={WIDTH} y1={TOP} y2={TOP} />
          <line className="fd-bars-base" x1="0" x2={WIDTH} y1={HEIGHT - 0.5} y2={HEIGHT - 0.5} />
          {dataFrom > 0 ? (
            <line
              className="fd-bars-start"
              x1={dataFrom * step}
              x2={dataFrom * step}
              y1="0"
              y2={HEIGHT}
            />
          ) : null}
          {bars.map((bar, i) => {
            if (i < dataFrom) return null;
            const h = Math.round((bar.value / top) * height);
            return (
              <rect
                key={bar.label}
                className="fd-bar"
                data-active={active === i ? 'true' : undefined}
                x={i * step + gap / 2}
                y={HEIGHT - h}
                width={Math.max(1, step - gap)}
                height={h}
                tabIndex={0}
                aria-label={`${bar.label}: ${format(bar.value)}`}
                onPointerEnter={() => setActive(i)}
                onPointerLeave={() => setActive(null)}
                onFocus={() => setActive(i)}
                onBlur={() => setActive(null)}
              />
            );
          })}
        </svg>
      </div>
      <div className="fd-bars-axis" aria-hidden="true">
        <span>{first?.label}</span>
        <span>{middle?.label}</span>
        <span>{last?.label}</span>
      </div>
      {dataFrom > 0 && dataSince !== null ? (
        <p className="fd-bars-nodata">No data before {monthDay(dataSince)}</p>
      ) : null}
      <table className="fd-vh">
        <caption>{`${metric} by ${by}`}</caption>
        <tbody>
          {bars.map((bar, i) => (
            <tr key={bar.label}>
              <th scope="row">{bar.label}</th>
              <td>{i < dataFrom ? 'No data' : format(bar.value)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
