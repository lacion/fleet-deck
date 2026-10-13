import type { DeckSession, StationView } from '../gen';
import { useNow } from '../ui/useNow';
import { costTotalText, fleetUsage, meterText, rateMeters, tokens, totalText } from './usage';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  /** Unix ms of now; tests give it. */
  readonly now?: number | undefined;
};

/** Fleet usage totals and provider rate-limit meters (DL-303). */
export function UsageMeters({ sessions, stations, now: given }: Props) {
  const now = useNow(given, 30000);
  const usage = fleetUsage(sessions);
  const meters = rateMeters(sessions, stations);
  return (
    <section className="fd-usage-meters" aria-label="Usage">
      <h2 className="fd-vh">Usage</h2>
      <dl className="fd-usage-stats">
        <div className="fd-usage-stat">
          <dt className="fd-label">Input tokens</dt>
          <dd>{totalText(usage.input, tokens)}</dd>
        </div>
        <div className="fd-usage-stat">
          <dt className="fd-label">Output tokens</dt>
          <dd>{totalText(usage.output, tokens)}</dd>
        </div>
        <div className="fd-usage-stat">
          <dt className="fd-label">Cost</dt>
          <dd>{costTotalText(usage)}</dd>
        </div>
      </dl>
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
    </section>
  );
}
