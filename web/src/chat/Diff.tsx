import { Fragment, useMemo, useState } from 'react';
import { CommentEditor } from '../review/CommentEditor';
import { arrange, rowIndex } from '../review/diffThreads';
import {
  type EditorState,
  editorTitle,
  nextEditor,
  RANGE_HINT,
  RANGE_TOO_LONG,
  rangeTooLong,
  spotLabel,
} from '../review/editor';
import { Thread } from '../review/Thread';
import { ThreadLists } from '../review/ThreadLists';
import type { DiffReview } from '../review/types';
import { diffLineKind } from './diffLines';

export type { DiffReview } from '../review/types';

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

/**
 * A unified diff with colored lines (UI-DESIGN §2.5). A file name opens the file when `onOpenFile` is given.
 * With `review` (DL-179), a line that can take a comment has its number as a button that opens the
 * comment editor, and the threads show under their last line.
 */
export function Diff({
  patch,
  truncated,
  id,
  onOpenFile,
  review,
}: {
  readonly patch: string;
  readonly truncated: boolean;
  readonly id: string;
  readonly onOpenFile?: ((path: string) => void) | undefined;
  readonly review?: DiffReview | undefined;
}) {
  const lines = patch.split('\n');
  const threads = review?.threads;
  const snapshot = review?.snapshot;
  const arranged = useMemo(
    () =>
      threads === undefined || snapshot === undefined
        ? null
        : arrange(patch, truncated, snapshot, threads),
    [patch, truncated, snapshot, threads],
  );
  const [editor, setEditor] = useState<{ state: EditorState; text: string } | null>(null);
  const editorAt =
    editor !== null && arranged !== null
      ? rowIndex(arranged.rows, editor.state.side, editor.state.path, editor.state.end)
      : undefined;
  const pre = (
    <pre className="fd-diff" data-diff={id}>
      {lines.map((line, n) => {
        const kind = diffLineKind(line);
        const file = kind === 'file' && onOpenFile ? splitDiffFileLine(line) : null;
        const row = review && arranged ? (arranged.rows[n] ?? null) : null;
        const here = arranged?.under.get(n);
        return (
          <Fragment key={n}>
            <span className="fd-dl" data-kind={kind}>
              {review && row ? (
                review.canWrite ? (
                  <button
                    type="button"
                    className="fd-dl-n"
                    aria-label={spotLabel(row)}
                    onClick={() =>
                      setEditor((now) => {
                        const next = nextEditor(now?.state ?? null, row, (now?.text ?? '') !== '');
                        return next === null
                          ? null
                          : next === now?.state
                            ? now
                            : { state: next, text: now?.text ?? '' };
                      })
                    }
                  >
                    {row.line}
                  </button>
                ) : (
                  <span className="fd-dl-n">{row.line}</span>
                )
              ) : null}
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
            {review && here
              ? here.map((t) => (
                  <Thread
                    key={t.thread_id}
                    thread={t}
                    env={review}
                    focused={review.focus === t.thread_id}
                    anchor={false}
                  />
                ))
              : null}
            {review && editor !== null && editorAt === n ? (
              <CommentEditor
                initial={editor.text}
                submitLabel="Add comment"
                title={editorTitle(editor.state)}
                hint={RANGE_HINT}
                blocked={rangeTooLong(editor.state) ? RANGE_TOO_LONG : undefined}
                onText={(text) => setEditor((now) => (now === null ? null : { ...now, text }))}
                onSubmit={(body) =>
                  review.onAdd(
                    {
                      side: editor.state.side,
                      path: editor.state.path,
                      line_start: editor.state.start,
                      line_end: editor.state.end,
                    },
                    body,
                  )
                }
                onClose={() => setEditor(null)}
              />
            ) : null}
          </Fragment>
        );
      })}
      {truncated ? (
        <span className="fd-dl" data-kind="meta">
          … (the diff is cut at the size limit)
        </span>
      ) : null}
    </pre>
  );
  if (!review || !arranged) return pre;
  return (
    <div className="fd-diffwrap">
      <ThreadLists arranged={arranged} review={review} />
      {pre}
    </div>
  );
}
