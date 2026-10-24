// B29: `#/analytics` and `#/analytics/<station>`: tokens and cost by time from the provider
// transcripts of each station, and the work of the Fleet Deck sessions (design analytics.md
// §3.10). The page reads once on open, on a filter change and on Refresh; it does not poll. A
// member sees what the deck filtered (`answer.owner` is false): no file counts, no station table.
import { type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { formatHash, type Route } from '../app/routes';
import { relativeAge } from '../app/sessions';
import { durationText } from '../chat/labels';
import type { AnalyticsSource, FleetAnalytics, StationAnalytics, StationView } from '../gen';
import { addedText, compactCount, countText, removedText, sizeText, usdText } from '../ui/format';
import { Icon, RefreshCw } from '../ui/icons';
import { Loading } from '../ui/Loading';
import { ErrorNotice, Page } from '../ui/page';
import { loadAnalytics } from './api';
import './analytics.css';
import { type Bar, Bars } from './Bars';
import { useAnalyticsGet } from './context';
import { learnings, monthDay } from './learnings';
import { HOUR_MS, type Merged, merge, type Period, type Totals, tokensOf, windowOf } from './merge';

type Props = {
  readonly route: Extract<Route, { name: 'analytics' }>;
  readonly stations: readonly StationView[];
  /** The answer at the first render (tests). */
  readonly initialAnswer?: FleetAnalytics | null;
  /** Unix ms of "now" (tests). */
  readonly now?: number;
  /** The time zone of the local days (tests); the browser's by default. */
  readonly zone?: string;
};

const PERIODS: readonly (readonly [Period, string])[] = [
  [1, 'Past 24 hours'],
  [7, '7 days'],
  [30, '30 days'],
  [90, '90 days'],
  [365, '365 days'],
  ['all', 'All time'],
];

type Metric = 'cost' | 'tokens';

const PROVIDER: Readonly<Record<string, string>> = { claude: 'Claude', codex: 'Codex' };
const providerName = (p: string): string => PROVIDER[p] ?? 'Provider';

const STATE_TEXT: Readonly<Record<string, string>> = {
  offline: 'Offline: not in the totals.',
  unsupported: 'Update this station to see its analytics.',
  busy: 'Busy: try again.',
  timeout: 'No answer in time.',
  error: 'The station could not answer.',
};

const TOOL_KINDS: readonly (readonly [string, string])[] = [
  ['shell', 'Shell'],
  ['edit', 'Edit'],
  ['read', 'Read'],
  ['search', 'Search'],
  ['web', 'Web'],
  ['mcp', 'MCP'],
  ['subagent', 'Subagent'],
  ['other', 'Other'],
];

const two = (n: number): string => String(n).padStart(2, '0');
/** `Oct 3, 14:00`: an hour start in the browser's zone. */
const hourText = (ms: number): string => `${monthDay(ms)}, ${two(new Date(ms).getHours())}:00`;
/** `Oct 3, 2026`. */
const dateText = (ms: number): string =>
  new Date(ms).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' });

const zoneOfBrowser = (): string => Intl.DateTimeFormat().resolvedOptions().timeZone;

function noPrice(t: Totals): boolean {
  return t.unpricedTokens > 0 && t.cost === 0;
}
const costText = (t: Totals): string => (noPrice(t) ? 'no price' : usdText(t.cost));

/** The sources of one station answer, in the order the station sent them (`missing` left out). */
const sourcesOf = (s: StationAnalytics): AnalyticsSource[] =>
  (s.answer?.sources ?? []).filter((src) => src.state !== 'missing');

export function AnalyticsPage({ route, stations, initialAnswer = null, now, zone }: Props) {
  const get = useAnalyticsGet();
  const [period, setPeriod] = useState<Period>(30);
  const [metric, setMetric] = useState<Metric>('cost');
  const [answer, setAnswer] = useState<FleetAnalytics | null>(initialAnswer);
  const [win, setWin] = useState<{ since: number; until: number }>(() =>
    windowOf(30, now ?? Date.now()),
  );
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(initialAnswer === null && get !== undefined);
  const [nonce, setNonce] = useState(0);
  const station = route.station;
  // The first render shows the answer of a test; a read starts when a filter changes.
  const skipFirst = useRef(initialAnswer !== null);

  useEffect(() => {
    if (skipFirst.current) {
      skipFirst.current = false;
      return;
    }
    if (get === undefined) return;
    let live = true;
    const next = windowOf(period, Date.now());
    setLoading(true);
    setError(null);
    loadAnalytics(get, next.since, next.until, station).then(
      (value) => {
        if (!live) return;
        setAnswer(value);
        setWin(next);
        setLoading(false);
      },
      (e: unknown) => {
        if (!live) return;
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      },
    );
    return () => {
      live = false;
    };
  }, [get, period, station, nonce]);

  const refresh = useCallback(() => setNonce((n) => n + 1), []);
  const merged = useMemo(
    () => (answer === null ? null : merge(answer, win.since, win.until, zone ?? zoneOfBrowser())),
    [answer, win, zone],
  );
  const member = answer !== null && !answer.owner;
  const clock = now ?? Date.now();

  const filters = (
    <>
      <select
        className="fd-ghost fd-sm"
        aria-label="Station"
        data-active={station === null ? undefined : 'true'}
        value={station ?? ''}
        onChange={(event) => {
          const name = event.target.value;
          window.location.hash = formatHash({
            name: 'analytics',
            station: name === '' ? null : name,
          });
        }}
      >
        <option value="">All stations</option>
        {stations.map((s) => (
          <option key={s.station_id} value={s.name}>
            {s.name}
          </option>
        ))}
      </select>
      <select
        className="fd-ghost fd-sm"
        aria-label="Period"
        value={String(period)}
        onChange={(event) => {
          const v = event.target.value;
          setPeriod(v === 'all' ? 'all' : (Number(v) as Period));
        }}
      >
        {PERIODS.map(([value, label]) => (
          <option key={value} value={String(value)}>
            {label}
          </option>
        ))}
      </select>
      <div className="fd-seg" role="group" aria-label="Chart metric">
        {(['cost', 'tokens'] as const).map((m) => (
          <button key={m} type="button" aria-pressed={metric === m} onClick={() => setMetric(m)}>
            {m === 'cost' ? 'Cost' : 'Tokens'}
          </button>
        ))}
      </div>
    </>
  );
  const actions = (
    <button
      type="button"
      className="fd-iconbtn"
      aria-label="Refresh analytics"
      disabled={loading}
      onClick={refresh}
    >
      <Icon of={RefreshCw} />
    </button>
  );

  return (
    <Page
      title="Analytics"
      description={
        <>
          Tokens and cost from the provider transcripts of each station, and the work of the Fleet
          Deck sessions.{member ? ' Your shared sessions only.' : ''}
        </>
      }
      filters={filters}
      actions={actions}
      wide
      className="fd-analytics"
    >
      <p className="fd-note fd-an-window">
        {win.since === 0
          ? 'All the history that the stations hold.'
          : `From ${hourText(win.since)} to now.`}
      </p>
      {error !== null ? <ErrorNotice text={error} onRetry={refresh} /> : null}
      {loading && merged === null ? <Loading text="Reading the analytics…" /> : null}
      {merged !== null && answer !== null ? (
        <Body
          answer={answer}
          merged={merged}
          metric={metric}
          member={member}
          fleet={station === null}
          now={clock}
          hourly={period === 1}
        />
      ) : null}
      {merged === null && !loading && error === null ? (
        <p className="fd-empty">
          No station answered. Analytics needs a station that is online and up to date.
        </p>
      ) : null}
    </Page>
  );
}

function Body({
  answer,
  merged,
  metric,
  member,
  fleet,
  now,
  hourly,
}: {
  readonly answer: FleetAnalytics;
  readonly merged: Merged;
  readonly metric: Metric;
  readonly member: boolean;
  readonly fleet: boolean;
  readonly now: number;
  readonly hourly: boolean;
}) {
  const answered = answer.stations.filter((s) => s.state === 'ok' && s.answer !== undefined);
  if (answered.length === 0) {
    return (
      <>
        <p className="fd-empty">
          No station answered. Analytics needs a station that is online and up to date.
        </p>
        {!member && fleet ? <StationTable answer={answer} merged={merged} /> : null}
      </>
    );
  }
  const list = learnings(merged);
  return (
    <>
      <TotalsPanel merged={merged} member={member} />
      <ByDay answer={answer} merged={merged} metric={metric} member={member} hourly={hourly} />
      <ByModel merged={merged} />
      {!member && fleet ? <StationTable answer={answer} merged={merged} /> : null}
      <Work merged={merged} />
      {list.length === 0 ? null : (
        <section className="fd-an-sec" aria-label="Learnings">
          <h2 className="fd-an-h">Learnings</h2>
          <ul className="fd-an-learn">
            {list.map((l) => (
              <li key={l.id} title={l.title}>
                {l.text}
              </li>
            ))}
          </ul>
        </section>
      )}
      <Sources answer={answer} merged={merged} member={member} now={now} />
    </>
  );
}

function Stat({
  label,
  value,
  title,
  tone,
}: {
  readonly label: string;
  readonly value: string;
  readonly title?: string;
  readonly tone?: 'fuel';
}) {
  return (
    <div className="fd-an-stat" title={title}>
      <span className="fd-an-value" data-tone={tone}>
        {value}
      </span>
      <span className="fd-an-label">{label}</span>
    </div>
  );
}

function TotalsPanel({ merged, member }: { readonly merged: Merged; readonly member: boolean }) {
  const t = merged.total;
  const run = merged.notInRun;
  const tokens = (n: number): string => compactCount(n);
  const prices = merged.priceTables.join(' and ');
  return (
    <section className="fd-an-sec" aria-label="Totals">
      <div className="fd-an-stats">
        <Stat label="Tokens" value={tokens(tokensOf(t))} title={countText(tokensOf(t))} />
        <Stat label="New input" value={tokens(t.input)} title={countText(t.input)} />
        <Stat label="Cache read" value={tokens(t.cacheRead)} title={countText(t.cacheRead)} />
        <Stat label="Cache write" value={tokens(t.cacheWrite)} title={countText(t.cacheWrite)} />
        <Stat label="Output" value={tokens(t.output)} title={countText(t.output)} />
        <Stat
          label="Cost estimate"
          value={usdText(t.cost)}
          tone="fuel"
          title={`At API list prices from the Fleet Deck price table of ${prices}. A subscription does not bill it. Models without a price are not in it.`}
        />
      </div>
      {t.unpricedTokens > 0 ? (
        <p className="fd-note">
          {countText(t.unpricedTokens)} tokens of {t.unpricedModels.join(', ')} have no price and
          are not in the cost.
        </p>
      ) : null}
      {!member && run !== null && tokensOf(run) > 0 ? (
        <p className="fd-note">
          Not in a Fleet Deck run (terminal, other apps, older or imported history):{' '}
          {countText(tokensOf(run))} tokens · {usdText(run.cost)}
        </p>
      ) : null}
    </section>
  );
}

/** The index of the first bar that holds the first record (`0`: no bar is before it). */
function firstWithData(
  bars: readonly { start: number; span: number }[],
  since: number | null,
): number {
  if (since === null) return 0;
  const i = bars.findIndex((b) => since < b.start + b.span);
  return i < 0 ? 0 : i;
}

function ByDay({
  answer,
  merged,
  metric,
  member,
  hourly,
}: {
  readonly answer: FleetAnalytics;
  readonly merged: Merged;
  readonly metric: Metric;
  readonly member: boolean;
  readonly hourly: boolean;
}) {
  const hours = hourly && merged.hours.length > 0;
  const entries = hours ? merged.hours : merged.days;
  const value = (t: Totals): number => (metric === 'cost' ? t.cost : tokensOf(t));
  const format = metric === 'cost' ? usdText : compactCount;
  const bars: Bar[] = entries.map((e) => ({
    label: hours ? hourText(e.start) : monthDay(e.start),
    value: value(e.totals),
  }));
  const dataFrom = firstWithData(
    entries.map((e, i) => ({
      start: e.start,
      span: hours ? HOUR_MS : (entries[i + 1]?.start ?? merged.window.until) - e.start,
    })),
    merged.usageSince,
  );
  const empty = tokensOf(merged.total) === 0 && merged.total.requests === 0;
  return (
    <section className="fd-an-sec" aria-label="By day">
      <h2 className="fd-an-h">By day</h2>
      {empty ? (
        <p className="fd-empty">No usage in this period.</p>
      ) : (
        <Bars
          metric={metric === 'cost' ? 'Cost' : 'Tokens'}
          bars={bars}
          format={format}
          total={value(merged.total)}
          dataFrom={dataFrom}
          dataSince={merged.usageSince}
          by={hours ? 'hour' : 'day'}
        />
      )}
      <Coverage answer={answer} merged={merged} member={member} />
    </section>
  );
}

/** The lines under the chart: where each provider's transcripts start, and the read progress. */
function Coverage({
  answer,
  merged,
  member,
}: {
  readonly answer: FleetAnalytics;
  readonly merged: Merged;
  readonly member: boolean;
}) {
  const lines: ReactNode[] = [];
  const stationsByName = [...answer.stations]
    .filter((s) => s.state === 'ok' && s.answer !== undefined)
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  for (const s of stationsByName) {
    for (const provider of ['claude', 'codex']) {
      const own = sourcesOf(s).filter(
        (src) => src.provider === provider && src.state === 'ok' && src.first_at !== null,
      );
      const start = own.reduce<AnalyticsSource | null>(
        (best, src) => (best === null || (src.first_at ?? 0) < (best.first_at ?? 0) ? src : best),
        null,
      );
      if (start === null || start.first_at === null) continue;
      const day = monthDay(start.first_at);
      let text: string;
      if (member) text = `Usage of your shared sessions on ${s.name} starts on ${day}.`;
      else if (provider === 'claude')
        text = `Claude transcripts on ${s.name} start on ${day}${
          start.keeps_days === null ? '' : ` (the CLI keeps them ${start.keeps_days} days)`
        }.`;
      else text = `Codex transcripts on ${s.name} start on ${day}.`;
      lines.push(
        <p key={`${s.station_id}-${provider}`} className="fd-note">
          {text}
        </p>,
      );
    }
    const mine = merged.stations.find((x) => x.id === s.station_id);
    if (member) {
      if (sourcesOf(s).some((src) => src.state === 'ok' && !src.complete))
        lines.push(
          <p key={`${s.station_id}-read`} className="fd-note">
            {s.name} is still reading its transcripts.
          </p>,
        );
    } else if (mine?.pending !== null && mine !== undefined && (mine.pending ?? 0) > 0) {
      lines.push(
        <p key={`${s.station_id}-read`} className="fd-note">
          Reading the transcripts of {s.name}: {sizeText(mine.pending ?? 0)} left.
        </p>,
      );
    }
  }
  return lines.length === 0 ? null : <div className="fd-an-lines">{lines}</div>;
}

function ByModel({ merged }: { readonly merged: Merged }) {
  const all = tokensOf(merged.total);
  if (merged.models.length === 0) return null;
  return (
    <section className="fd-an-sec" aria-label="By model">
      <h2 className="fd-an-h">By model</h2>
      <div className="fd-tablewrap">
        <table className="fd-table">
          <thead>
            <tr>
              <th>Model</th>
              <th>Provider</th>
              <th className="fd-an-num">Tokens</th>
              <th>Share</th>
              <th className="fd-an-num">Cost estimate</th>
            </tr>
          </thead>
          <tbody>
            {merged.models.map((m) => {
              const tokens = tokensOf(m.totals);
              const share = all > 0 ? (tokens / all) * 100 : 0;
              return (
                <tr key={`${m.provider}|${m.model}`}>
                  <td className="fd-cell-name">{m.model}</td>
                  <td>{providerName(m.provider)}</td>
                  <td className="fd-an-num">{countText(tokens)}</td>
                  <td className="fd-an-share">
                    <span
                      className="fd-meter"
                      role="img"
                      aria-label={`${Math.round(share)} % of the tokens`}
                    >
                      <span style={{ width: `${share}%` }} />
                    </span>
                  </td>
                  <td className="fd-an-num">{costText(m.totals)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function StationTable({
  answer,
  merged,
}: {
  readonly answer: FleetAnalytics;
  readonly merged: Merged;
}) {
  return (
    <section className="fd-an-sec" aria-label="By station">
      <h2 className="fd-an-h">By station</h2>
      <div className="fd-tablewrap">
        <table className="fd-table">
          <thead>
            <tr>
              <th>Station</th>
              <th className="fd-an-num">Tokens</th>
              <th className="fd-an-num">Cost estimate</th>
              <th>State</th>
            </tr>
          </thead>
          <tbody>
            {merged.stations.map((s) => {
              const raw = answer.stations.find((x) => x.station_id === s.id);
              const off = raw !== undefined && sourcesOf(raw).some((src) => src.state === 'off');
              const text =
                STATE_TEXT[s.state] ??
                (off ? 'Transcript reading is off (Settings → station).' : '');
              return (
                <tr key={s.id}>
                  <td className="fd-cell-name">{s.name}</td>
                  <td className="fd-an-num">
                    {s.totals === null ? '' : countText(tokensOf(s.totals))}
                  </td>
                  <td className="fd-an-num">{s.totals === null ? '' : costText(s.totals)}</td>
                  <td>{text}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function Row({
  label,
  value,
  share,
  extra,
}: {
  readonly label: string;
  readonly value: string;
  /** A neutral meter of the share (0 to 100). */
  readonly share?: number;
  readonly extra?: string;
}) {
  return (
    <li className="fd-an-row">
      <span className="fd-an-rowlabel">{label}</span>
      <span className="fd-an-rowvalue">{value}</span>
      {share === undefined ? null : (
        <span
          className="fd-meter"
          data-tone="neutral"
          role="img"
          aria-label={`${Math.round(share)} % of the calls`}
        >
          <span style={{ width: `${share}%` }} />
        </span>
      )}
      {extra === undefined || extra === '' ? null : <span className="fd-an-extra">{extra}</span>}
    </li>
  );
}

function Work({ merged }: { readonly merged: Merged }) {
  const n = (key: string): number => merged.counts.get(key) ?? 0;
  const sum = (prefix: string): number =>
    [...merged.counts].reduce((s, [key, v]) => (key.startsWith(prefix) ? s + v : s), 0);
  const { keptSince, eventsSince, window } = merged;
  const none = 'No data in this period.';
  const kept = keptSince !== null && keptSince <= window.until;
  const events = eventsSince !== null && eventsSince <= window.until;
  const calls = TOOL_KINDS.reduce((s, [kind]) => s + n(`tools.${kind}`), 0);
  const waited = n('approvals.waited');
  const timed = n('turns.timed');
  const failedShell = n('shell.failed');
  return (
    <section className="fd-an-sec" aria-label="Fleet Deck work">
      <h2 className="fd-an-h">Fleet Deck work</h2>
      {kept ? (
        <>
          <p className="fd-note">Since {dateText(keptSince)}.</p>
          <ul className="fd-an-rows">
            <Row label="Sessions started" value={countText(n('sessions.created'))} />
            <Row label="Turns" value={countText(n('turns.recorded'))} />
            <Row
              label="Shell commands"
              value={`${countText(n('shell.commands'))}${failedShell > 0 ? `, ${countText(failedShell)} failed` : ''}`}
            />
          </ul>
        </>
      ) : (
        <p className="fd-note">{none}</p>
      )}
      {events ? (
        <>
          <p className="fd-note">
            Since {dateText(eventsSince)}. The station keeps its events 7 days; Fleet Deck counts
            them from then on.
          </p>
          <ul className="fd-an-rows">
            <Row
              label="Turns that ended"
              value={`${countText(n('turns.done'))} done · ${countText(n('turns.interrupted'))} interrupted · ${countText(n('turns.failed'))} failed`}
            />
            <Row
              label="Average turn"
              value={timed > 0 ? durationText(n('turns.ms') / timed) : '0'}
            />
            <Row label="Tool calls" value={countText(calls)} />
            {TOOL_KINDS.map(([kind, label]) => {
              const count = n(`tools.${kind}`);
              const failed = n(`tools.failed.${kind}`);
              const denied = n(`tools.denied.${kind}`);
              const extra = [
                failed > 0 ? `${countText(failed)} failed` : '',
                denied > 0 ? `${countText(denied)} denied` : '',
              ]
                .filter((x) => x !== '')
                .join(' · ');
              return (
                <Row
                  key={kind}
                  label={label}
                  value={countText(count)}
                  share={calls > 0 ? (count / calls) * 100 : 0}
                  extra={extra}
                />
              );
            })}
            <Row
              label="Approvals"
              value={`${countText(n('approvals.asked'))} asked · ${countText(sum('approvals.answered.'))} answered · ${countText(n('approvals.expired'))} expired`}
            />
            <Row
              label="Average wait for an answer"
              value={waited > 0 ? durationText(n('approvals.wait_ms') / waited) : '0'}
            />
            <Row label="Resumes" value={countText(n('resumes'))} />
            <Row label="Files changed" value={countText(n('files.changed'))} />
            <Row
              label="Lines"
              value={`${addedText(n('lines.added'))} ${removedText(n('lines.removed'))}`}
            />
            <Row label="Artifacts published" value={countText(n('artifacts'))} />
            <Row label="Workflows" value={countText(n('workflows'))} />
            <Row label="Errors" value={countText(n('errors'))} />
          </ul>
        </>
      ) : (
        <p className="fd-note">{none}</p>
      )}
    </section>
  );
}

function Sources({
  answer,
  merged,
  member,
  now,
}: {
  readonly answer: FleetAnalytics;
  readonly merged: Merged;
  readonly member: boolean;
  readonly now: number;
}) {
  const stationList = [...answer.stations]
    .filter((s) => s.answer !== undefined)
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  const lines: string[] = [];
  for (const s of stationList) {
    for (const src of sourcesOf(s)) {
      const head = `${s.name} · ${providerName(src.provider)}`;
      const since = src.first_at === null ? '' : `since ${dateText(src.first_at)}`;
      if (src.state === 'ok') {
        if (member) lines.push(`${head}: ${since === '' ? 'no records yet' : since}`);
        else {
          const parts = [`${countText(src.files ?? 0)} files`];
          if (since !== '') parts.push(since);
          if (src.scanned_at !== null) parts.push(`read ${relativeAge(src.scanned_at, now)} ago`);
          lines.push(`${head}: ${parts.join(', ')}`);
        }
      } else if (src.state === 'off') lines.push(`${head}: transcript reading is off`);
      else if (member || src.state !== 'unavailable') lines.push(`${head}: not available`);
      else lines.push(`${head}: not available (${src.reason})`);
    }
  }
  const truncated = new Set<string>();
  const partial = new Set<number>();
  for (const s of stationList) {
    if (s.truncated_before !== undefined)
      truncated.add(
        `Hours before ${hourText(s.truncated_before)} are not shown: the answer of ${s.name} was too large.`,
      );
    for (const h of s.partial_hours ?? []) partial.add(h);
  }
  const prices =
    merged.priceTables.length > 1
      ? merged.priceTables
          .map((table) => {
            const names = stationList
              .filter((s) => s.answer?.price_table === table)
              .map((s) => s.name);
            return `${table} (${names.join(', ')})`;
          })
          .join(' and ')
      : (merged.priceTables[0] ?? '');
  return (
    <details className="fd-an-sources">
      <summary>Sources</summary>
      <ul className="fd-an-rows">
        {lines.map((l) => (
          <li key={l}>{l}</li>
        ))}
        {prices === '' ? null : <li>Prices of {prices}</li>}
        {merged.duplicateSources.length === 0 ? null : (
          <li>Counted once per hour and conversation: {merged.duplicateSources.join('; ')}</li>
        )}
        {[...truncated].map((t) => (
          <li key={t}>{t}</li>
        ))}
        {[...partial]
          .sort((a, b) => a - b)
          .map((h) => (
            <li key={h}>Some rows of {hourText(h)} are not shown.</li>
          ))}
      </ul>
    </details>
  );
}
