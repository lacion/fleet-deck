import { diffLineKind } from './diffLines';

/** A unified diff with colored lines (UI-DESIGN §2.5). */
export function Diff({
  patch,
  truncated,
  id,
}: {
  readonly patch: string;
  readonly truncated: boolean;
  readonly id: string;
}) {
  const lines = patch.split('\n');
  return (
    <pre className="fd-diff" data-diff={id}>
      {lines.map((line, n) => (
        <span key={n} className="fd-dl" data-kind={diffLineKind(line)}>
          {line === '' ? ' ' : line}
        </span>
      ))}
      {truncated ? (
        <span className="fd-dl" data-kind="meta">
          … (the diff is cut at the size limit)
        </span>
      ) : null}
    </pre>
  );
}
