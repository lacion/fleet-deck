import type { CliStatus, IntegrationsStatus, ProjectPreflight } from '../gen';
import { systemLabel } from './model';

const cliLine = (name: string, cli: CliStatus): string => {
  if (!cli.present) return `${name}: not installed on the station`;
  if (cli.authenticated) return `${name}: signed in${cli.user === null ? '' : ` as ${cli.user}`}`;
  return `${name}: not signed in${cli.message === '' ? '' : `. ${cli.message}`}`;
};

/** The status lines of a station: its CLIs and its tracker credentials. */
export function statusLines(status: IntegrationsStatus): string[] {
  return [
    cliLine('gh', status.gh),
    cliLine('glab', status.glab),
    ...status.trackers.map(
      (t) =>
        `tracker ${t.id} (${systemLabel(t.kind)}${t.site === null ? '' : ` at ${t.site}`}): ${
          t.ok
            ? `token accepted${t.user === null ? '' : ` as ${t.user}`}`
            : `not usable. ${t.message}`
        }`,
    ),
  ];
}

/** The result of "Check": each binding check with ok or fail, and the status lines. */
export function PreflightView({ result }: { readonly result: ProjectPreflight }) {
  return (
    <div className="fd-int-preflight" aria-label="Check result">
      <ul>
        {result.checks.map((c) => (
          <li key={c.label} data-ok={c.ok}>
            <span className="fd-badge" data-tone={c.ok ? 'ok' : 'hazard'}>
              {c.ok ? 'ok' : 'fail'}
            </span>{' '}
            {c.message}
          </li>
        ))}
      </ul>
      {result.status === null ? (
        <p className="fd-note">The station did not report its status.</p>
      ) : (
        <ul className="fd-int-status">
          {statusLines(result.status).map((line) => (
            <li key={line} className="fd-note">
              {line}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
