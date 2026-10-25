import type { DeckSession, StationView } from '../gen';
import { useNow } from '../ui/useNow';
import { UsageRecount } from './UsageRecount';
import type { RecountCall } from './usageRecountCall';
import {
  API_PRICES,
  apiPricesTitle,
  assistMeters,
  assistText,
  costTotalText,
  fleetUsage,
  inputLabel,
  meterText,
  rateMeters,
  tokens,
  totalText,
} from './usage';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  /** Unix ms of now; tests give it. */
  readonly now?: number | undefined;
  /** The owner's recount of old counts (B20); absent for any other login. */
  readonly recount?: RecountCall | undefined;
};

const sentence = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

/** Fleet usage totals and provider rate-limit meters (DL-303). */
export function UsageMeters({ sessions, stations, now: given, recount }: Props) {
  const now = useNow(given, 30000);
  const usage = fleetUsage(sessions);
  const meters = rateMeters(sessions, stations);
  const assist = assistMeters(stations);
  return (
    <section className="fd-usage-meters" aria-label="Usage">
      <h2 className="fd-vh">Usage</h2>
      <dl className="fd-usage-stats">
        <div className="fd-usage-stat">
          <dt className="fd-label">{`${sentence(inputLabel())} tokens`}</dt>
          <dd>{totalText(usage.input, tokens)}</dd>
        </div>
        <div className="fd-usage-stat">
          <dt className="fd-label">Cache read tokens</dt>
          <dd>{totalText(usage.cacheRead, tokens)}</dd>
        </div>
        <div className="fd-usage-stat">
          <dt className="fd-label">Cache write tokens</dt>
          <dd>{totalText(usage.cacheWrite, tokens)}</dd>
        </div>
        <div className="fd-usage-stat">
          <dt className="fd-label">Output tokens</dt>
          <dd>{totalText(usage.output, tokens)}</dd>
        </div>
        <div className="fd-usage-stat">
          <dt className="fd-label" title={apiPricesTitle(sessions.length)}>
            {sentence(API_PRICES)}
          </dt>
          <dd>{costTotalText(usage)}</dd>
        </div>
      </dl>
      {usage.oldCounts === 0 ? null : (
        <p className="fd-note">
          {usage.oldCounts === 1
            ? '1 session has an old count that is not in these totals.'
            : `${usage.oldCounts} sessions have old counts that are not in these totals.`}
        </p>
      )}
      {recount === undefined ? null : <UsageRecount sessions={sessions} recount={recount} />}
      {meters.length === 0 ? (
        <p className="fd-note">No provider reported a rate limit yet.</p>
      ) : (
        <ul className="fd-usage-meterlist">
          {meters.map((m) => (
            <li key={m.key} className="fd-usage-meter">
              <span className="fd-usage-name">
                {m.label} · {m.scope}
              </span>
              {m.usedPercent === null ? (
                <span />
              ) : (
                <meter
                  min={0}
                  max={100}
                  high={80}
                  value={m.usedPercent}
                  aria-label={`${m.label} ${m.scope}`}
                />
              )}
              <span className="fd-usage-text">{meterText(m, now)}</span>
            </li>
          ))}
        </ul>
      )}
      {assist.length === 0 ? null : (
        <ul className="fd-usage-meterlist" aria-label="Fleet Deck model help">
          {assist.map((m) => (
            <li key={m.key} className="fd-usage-meter" data-scope="fleetdeck">
              <span className="fd-usage-name">{m.label}</span>
              <meter
                min={0}
                max={m.cap}
                high={m.cap * 0.8}
                value={Math.min(m.tokens, m.cap)}
                aria-label={m.label}
              />
              <span className="fd-usage-text">{assistText(m)}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
