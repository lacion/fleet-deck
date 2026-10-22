// The Usage section at the top of the fleet home's ops panel (DESIGN-SYSTEM §4.4): the tightest
// rate-limit windows as fuel meters and the fleet totals. The Usage tab keeps every meter and the
// unknown parts; this glance shows only what is known.
import type { DeckSession, StationView } from '../gen';
import { compactCount } from '../ui/format';
import { Fuel, Icon } from '../ui/icons';
import { useNow } from '../ui/useNow';
import {
  dollars,
  fleetUsage,
  inputLabel,
  rateMeters,
  resetsText,
  scopeText,
  tokens,
  WARN_PERCENT,
} from './usage';

/** How many windows the glance shows (the tightest first). */
const SHOWN = 3;

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
  const known = rateMeters(sessions, stations).filter((m) => m.usedPercent !== null);
  const meters = known.slice(0, SHOWN);
  // The station name tells two accounts apart only when several stations report limits.
  const manyStations = new Set(known.map((m) => m.stationId)).size > 1;
  const usage = fleetUsage(sessions);
  const approx = usage.costInferred ? '≈ ' : '';
  // Each total has its exact value as its title: the row shows a count short ("1.5M").
  const totals = [
    { label: inputLabel(sessions), value: usage.input.known, text: compactCount, exact: tokens },
    { label: 'output tokens', value: usage.output.known, text: compactCount, exact: tokens },
    {
      label: 'cost',
      value: usage.cost.known,
      text: (n: number) => `${approx}${dollars(n)}`,
      exact: (n: number) => `${approx}${dollars(n)}`,
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
                  {m.provider} <span className="fd-glance-scope">{scopeText(m.scope)}</span>
                  {manyStations ? <span className="fd-glance-station"> · {m.station}</span> : null}
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
        <>
          {/* The scope of the totals: the shown sessions, since they started. */}
          <p className="fd-glance-span">
            {sessions.length === 1 ? '1 session' : `${sessions.length} sessions`} · all time
          </p>
          <dl className="fd-glance-totals">
            {totals.map((t) => (
              <div key={t.label}>
                <dt>{t.label}</dt>
                <dd title={t.value === null ? undefined : t.exact(t.value)}>
                  {t.value === null ? null : t.text(t.value)}
                </dd>
              </div>
            ))}
          </dl>
        </>
      )}
    </div>
  );
}
