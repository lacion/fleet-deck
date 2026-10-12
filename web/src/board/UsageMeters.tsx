import type { DeckSession, StationView } from '../gen';
import { dollars, fleetUsage, meterText, rateMeters, tokens, totalText } from './usage';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
};

/** Fleet usage totals and provider rate-limit meters (DL-303). */
export function UsageMeters({ sessions, stations }: Props) {
  const usage = fleetUsage(sessions);
  const meters = rateMeters(sessions, stations);
  return (
    <section className="fd-usage-meters" aria-label="Usage">
      <h2>Usage</h2>
      <dl>
        <dt>Input tokens</dt>
        <dd>{totalText(usage.input, tokens)}</dd>
        <dt>Output tokens</dt>
        <dd>{totalText(usage.output, tokens)}</dd>
        <dt>Cost</dt>
        <dd>{totalText(usage.cost, dollars)}</dd>
      </dl>
      {meters.length === 0 ? (
        <p className="fd-note">No provider reported a rate limit yet.</p>
      ) : (
        <ul>
          {meters.map((m) => (
            <li key={m.key}>
              <span>
                {m.label} · {m.scope}
              </span>{' '}
              {m.usedPercent === null ? null : (
                <meter
                  min={0}
                  max={100}
                  high={80}
                  value={m.usedPercent}
                  aria-label={`${m.label} ${m.scope}`}
                />
              )}{' '}
              <span>{meterText(m)}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
