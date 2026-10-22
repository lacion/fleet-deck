// The Usage section at the top of the fleet home's ops panel (DESIGN-SYSTEM §4.4): the tightest
// rate-limit windows as fuel meters and the fleet totals. The Usage tab keeps every meter and the
// unknown parts; this glance shows only what is known.
import type { DeckSession, StationView } from '../gen';
import { Fuel, Icon } from '../ui/icons';
import { useNow } from '../ui/useNow';
import { dollars, fleetUsage, rateMeters, resetsText, WARN_PERCENT } from './usage';

/** How many windows the glance shows (the tightest first). */
const SHOWN = 3;

const SCOPE_TEXT: Readonly<Record<string, string>> = {
  five_hour: '5-hour window',
  weekly: 'weekly',
};

/** "4.2 M", "610 k", "940". */
export function compactCount(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)} M`;
  if (n >= 10_000) return `${Math.round(n / 1000)} k`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)} k`;
  return String(n);
}

const sentence = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

export function UsageGlance({
  sessions,
  stations,
  now: given,
}: {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly now?: number | undefined;
}) {
  const now = useNow(given, 30000);
  const meters = rateMeters(sessions, stations)
    .filter((m) => m.usedPercent !== null)
    .slice(0, SHOWN);
  const usage = fleetUsage(sessions);
  const totals = [
    { label: 'input tokens', value: usage.input.known, text: compactCount },
    { label: 'output tokens', value: usage.output.known, text: compactCount },
    {
      label: 'cost',
      value: usage.cost.known,
      text: (n: number) => `${usage.costInferred ? '≈ ' : ''}${dollars(n)}`,
    },
  ].filter((t) => t.value !== null);
  if (meters.length === 0 && totals.length === 0) return null;
  return (
    <div className="fd-ops-sec fd-glance">
      <h2 className="fd-ops-h">
        <Icon of={Fuel} size={14} /> Usage
      </h2>
      {meters.length === 0 ? null : (
        <ul className="fd-glance-meters">
          {meters.map((m) => {
            const used = m.usedPercent ?? 0;
            return (
              <li key={m.key} className="fd-glance-meter">
                <span className="fd-glance-name">
                  {m.label}{' '}
                  <span className="fd-glance-scope">{SCOPE_TEXT[m.scope] ?? m.scope}</span>
                </span>
                <span className="fd-glance-pct">{Math.round(used)}%</span>
                <span
                  className="fd-meter"
                  data-level={used >= WARN_PERCENT ? 'high' : undefined}
                  aria-hidden="true"
                >
                  <span style={{ width: `${Math.min(Math.max(used, 0), 100)}%` }} />
                </span>
                {m.resetsAt === null ? null : (
                  <span className="fd-glance-reset">{sentence(resetsText(m.resetsAt, now))}</span>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {totals.length === 0 ? null : (
        <dl className="fd-glance-totals">
          {totals.map((t) => (
            <div key={t.label}>
              <dt>{t.label}</dt>
              <dd>{t.value === null ? null : t.text(t.value)}</dd>
            </div>
          ))}
        </dl>
      )}
    </div>
  );
}
