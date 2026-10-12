import { diffLineKind } from './diffLines';

const FILE_LINE = /^(diff --git a\/.* b\/)(.+)$/;

/** The parts of a `diff --git a/X b/Y` line: the text before the path Y, and Y; `null` for another shape. */
export function splitDiffFileLine(
  line: string,
): { readonly head: string; readonly path: string } | null {
  const match = FILE_LINE.exec(line);
  return match?.[1] !== undefined && match[2] !== undefined
    ? { head: match[1], path: match[2] }
    : null;
}

/** A unified diff with colored lines (UI-DESIGN §2.5). A file name opens the file when `onOpenFile` is given. */
export function Diff({
  patch,
  truncated,
  id,
  onOpenFile,
}: {
  readonly patch: string;
  readonly truncated: boolean;
  readonly id: string;
  readonly onOpenFile?: ((path: string) => void) | undefined;
}) {
  const lines = patch.split('\n');
  return (
    <pre className="fd-diff" data-diff={id}>
      {lines.map((line, n) => {
        const kind = diffLineKind(line);
        const file = kind === 'file' && onOpenFile ? splitDiffFileLine(line) : null;
        return (
          <span key={n} className="fd-dl" data-kind={kind}>
            {file ? (
              <>
                {file.head}
                <button
                  type="button"
                  className="fd-dl-file"
                  onClick={() => onOpenFile?.(file.path)}
                >
                  {file.path}
                </button>
              </>
            ) : line === '' ? (
              ' '
            ) : (
              line
            )}
          </span>
        );
      })}
      {truncated ? (
        <span className="fd-dl" data-kind="meta">
          … (the diff is cut at the size limit)
        </span>
      ) : null}
    </pre>
  );
}
