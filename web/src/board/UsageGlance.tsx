// The Usage section at the top of the fleet home's ops panel (DESIGN-SYSTEM §4.4): the tightest
// rate-limit windows as fuel meters and the fleet totals. The Usage tab keeps every meter and the
// unknown parts; this glance shows only what is known.
import type { DeckSession, StationView } from '../gen';
import { compactCount } from '../ui/format';
import { Fuel, Icon } from '../ui/icons';
import { useNow } from '../ui/useNow';
import { isLiveSession } from './fleet';
import {
  API_PRICES,
  apiPricesTitle,
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

/**
 * What the totals count, in the words of the header sentence: "7 live and 2 ended sessions" (the
 * header says "7 live sessions"; the ended ones are the Ended folds). A part of zero is left out;
 * the noun agrees with the last number ("1 live session", "6 live and 1 ended session").
 */
export function sessionsText(sessions: readonly DeckSession[]): string {
  const live = sessions.filter(isLiveSession).length;
  const away = sessions.filter((s) => s.status === 'unreachable').length;
  const ended = sessions.length - live - away;
  const parts = [
    { n: live, word: 'live' },
    { n: away, word: 'unreachable' },
    { n: ended, word: 'ended' },
  ].filter((p) => p.n > 0);
  const last = parts.at(-1);
  if (last === undefined) return '0 sessions';
  const words = parts.map((p) => `${p.n} ${p.word}`);
  const head = words.slice(0, -1).join(', ');
  const noun = last.n === 1 ? 'session' : 'sessions';
  return `${head === '' ? '' : `${head} and `}${words.at(-1)} ${noun}`;
}

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
  // Each total has its exact value as its title: the row shows a count short ("1.5M"). The cost is
  // the price at the provider's API rates (cached input included), named and explained as such.
  const priced = apiPricesTitle(sessions.length);
  const totals = [
    { label: inputLabel(sessions), value: usage.input.known, text: compactCount, exact: tokens },
    { label: 'output', value: usage.output.known, text: compactCount, exact: tokens },
    {
      label: API_PRICES,
      value: usage.cost.known,
      text: (n: number) => `${approx}${dollars(n)}`,
      exact: (n: number) => `${approx}${dollars(n)}\n${priced}`,
      about: priced,
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
          {/* The scope of the totals: the sessions on the board (the archived ones only while
              Filters shows them), each since it started, in the live words of the header. */}
          <p className="fd-glance-span">{sessionsText(sessions)}</p>
          <dl className="fd-glance-totals">
            {totals.map((t) => (
              <div key={t.label}>
                <dt title={'about' in t ? t.about : undefined}>{t.label}</dt>
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
