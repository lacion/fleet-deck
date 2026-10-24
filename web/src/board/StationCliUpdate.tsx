// B28 (design `docs/v2/design/cli-update.md` §3.12): the provider update lines of a station card,
// under the provider list. One line per provider, then the schedule line; the owner gets the link
// to the Settings card. Shown only when the station reports `cli_update`.
import { formatHash } from '../app/routes';
import type { CliUpdateReport } from '../gen';
import { type LineView, providerLine, scheduleText } from '../settings/cliUpdate';

/** One line: quiet text, the hazard color of the provider line, or plain text. */
export function UpdateLine({ line }: { readonly line: LineView }) {
  if (line.tone === 'hazard') {
    return (
      <p className="fd-note" data-tone="hazard">
        <span className="fd-station-auth" data-auth="hazard">
          {line.text}
        </span>
      </p>
    );
  }
  return line.tone === 'normal' ? (
    <p data-tone="normal">{line.text}</p>
  ) : (
    <p className="fd-note" data-tone="quiet">
      {line.text}
    </p>
  );
}

export function StationCliUpdate({
  report,
  stationId,
  canAdmin,
  now,
}: {
  readonly report: CliUpdateReport;
  readonly stationId: string;
  readonly canAdmin: boolean;
  readonly now: number;
}) {
  return (
    <div className="fd-station-cli-update" aria-label="Provider updates">
      {report.providers.map((state) => {
        const line = providerLine(state, report.running, now);
        return line === null ? null : <UpdateLine key={state.provider} line={line} />;
      })}
      <p className="fd-note" data-tone="quiet">
        {scheduleText(report)}
        {canAdmin ? (
          <>
            {' · '}
            <a
              className="fd-station-signin fd-tap"
              href={formatHash({
                name: 'settings',
                tab: 'station',
                station: stationId,
              })}
            >
              Updates…
            </a>
          </>
        ) : null}
      </p>
    </div>
  );
}
