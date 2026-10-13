import type { PreflightResult } from './spawn';

type Props = {
  readonly url: string;
  readonly checking: boolean;
  /** The result for this station and URL, or `null` (none yet, or the URL changed). */
  readonly result: PreflightResult | null;
  readonly onCheck: () => void;
};

/** DL-144: the "Check access" button of a clone URL and its result line. The check never blocks the start. */
export function CloneAccess({ url, checking, result, onCheck }: Props) {
  return (
    <>
      <button
        type="button"
        className="fd-ghost fd-sm fd-spawn-check"
        disabled={url.trim() === '' || checking}
        onClick={onCheck}
      >
        {checking ? 'Checking…' : 'Check access'}
      </button>
      {result === null ? null : result.ok ? (
        result.preflight.ok ? (
          <p className="fd-spawn-access" role="status">
            <span className="fd-badge" data-tone="ok">
              Access OK
            </span>{' '}
            The station can read this repository.
          </p>
        ) : (
          <div className="fd-spawn-access">
            <p role="alert">
              {result.preflight.hint ?? 'The station cannot read this repository.'}
            </p>
            {result.preflight.message === '' ? null : (
              <details>
                <summary>Git said</summary>
                <pre className="fd-code">{result.preflight.message}</pre>
              </details>
            )}
          </div>
        )
      ) : (
        <p className="fd-spawn-access" role="alert">
          {result.text}
        </p>
      )}
    </>
  );
}
