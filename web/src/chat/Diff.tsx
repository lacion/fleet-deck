import { type CSSProperties, Fragment, type ReactNode, useMemo, useState } from 'react';
import { CommentEditor } from '../review/CommentEditor';
import type { DiffRow } from '../review/diffModel';
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
import { addedText, removedText } from '../ui/format';
import { FileText, Icon } from '../ui/icons';
import {
  diffLineKinds,
  type FileHead,
  type FileStatus,
  fileHeads,
  isPlumbing,
  type LineNo,
  lineNumbers,
} from './diffLines';

export type { DiffReview } from '../review/types';
export { splitDiffFileLine } from './diffLines';

/** The narrowest number gutter, in digits. */
const GUTTER_MIN = 3;

/** A path as its folders (they give way to an ellipsis first) and its file name. */
function PathText({ path }: { readonly path: string }) {
  const cut = path.lastIndexOf('/') + 1;
  return (
    <>
      {cut > 0 ? (
        <span className="fd-dl-dir">
          <bdi>{path.slice(0, cut)}</bdi>
        </span>
      ) : null}
      <span className="fd-dl-base">{path.slice(cut)}</span>
    </>
  );
}

/** The text and the tone of the status badge of a file header. */
export function fileStatusBadge(status: FileStatus): {
  readonly text: string;
  readonly title: string;
  readonly tone: 'ok' | 'hazard' | undefined;
} {
  switch (status.kind) {
    case 'added':
      return { text: 'new', title: 'New file', tone: 'ok' };
    case 'deleted':
      return { text: 'deleted', title: 'Deleted file', tone: 'hazard' };
    case 'mode':
      return {
        text: status.new === '' ? `mode ${status.old}` : `mode ${status.old} \u2192 ${status.new}`,
        title: 'The file mode changed',
        tone: undefined,
      };
  }
}

/** The status badge of a file header: a new or a deleted file, or a changed file mode. */
export function FileStatusBadge({ status }: { readonly status: FileStatus | null }) {
  if (status === null) return null;
  const badge = fileStatusBadge(status);
  return (
    <span className="fd-badge fd-dl-status" data-tone={badge.tone} title={badge.title}>
      {badge.text}
    </span>
  );
}

/** The header row of a file: icon, path (a button with `onOpenFile`), its status and its +/− counts. */
function FileHeader({
  head,
  onOpenFile,
}: {
  readonly head: FileHead;
  readonly onOpenFile: ((path: string) => void) | undefined;
}) {
  return (
    <span className="fd-dl" data-kind="file">
      <Icon of={FileText} size={14} />
      {onOpenFile ? (
        // The label keeps the path whole: the two parts are flex items (a screen reader would
        // read a space between them).
        <button
          type="button"
          className="fd-dl-file"
          aria-label={head.path}
          onClick={() => onOpenFile(head.path)}
        >
          <PathText path={head.path} />
        </button>
      ) : (
        <span className="fd-dl-path">
          <PathText path={head.path} />
        </span>
      )}
      <FileStatusBadge status={head.status} />
      {head.add > 0 || head.del > 0 ? (
        <span className="fd-dl-stat">
          {head.add > 0 ? <span data-sign="add">{addedText(head.add)}</span> : null}
          {head.del > 0 ? <span data-sign="del">{removedText(head.del)}</span> : null}
        </span>
      ) : null}
    </span>
  );
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
  const lines = useMemo(() => patch.split('\n'), [patch]);
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
  const kinds = useMemo(() => diffLineKinds(lines), [lines]);
  const heads = useMemo(() => fileHeads(lines), [lines]);
  // DL-179: with review, each hunk line shows its old and its new number in two gutters.
  const numbered = review !== undefined && arranged !== null;
  const numbers = useMemo(() => (numbered ? lineNumbers(patch) : null), [patch, numbered]);
  const digits = useMemo(() => {
    let most = 0;
    for (const n of numbers?.values() ?? []) most = Math.max(most, n.old ?? 0, n.new ?? 0);
    return Math.max(GUTTER_MIN, String(most).length);
  }, [numbers]);
  const target = (row: NonNullable<DiffRow>): ReactNode =>
    review?.canWrite ? (
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
    );
  /** The old and the new gutter: the comment target sits in the gutter of its side. */
  const gutters = (row: DiffRow, no: LineNo | undefined): ReactNode => (
    <>
      <span className="fd-dl-g" data-side="old">
        {row?.side === 'old' ? target(row) : (no?.old ?? null)}
      </span>
      <span className="fd-dl-g" data-side="new">
        {row?.side === 'new' ? target(row) : (no?.new ?? null)}
      </span>
    </>
  );
  const style = numbers ? ({ '--dl-digits': `${digits}ch` } as CSSProperties) : undefined;
  const pre = (
    <pre className="fd-diff" data-diff={id} data-numbers={numbers ? '' : undefined} style={style}>
      {lines.map((line, n) => {
        const kind = kinds[n] ?? 'ctx';
        const head = kind === 'file' ? heads.get(n) : undefined;
        const row = review && arranged ? (arranged.rows[n] ?? null) : null;
        const here = arranged?.under.get(n);
        // The git plumbing (index, ---, +++) of a file header is left out: the file header row
        // says the same. In a hunk every line shows (`diffLineKinds` makes its -/+ lines del/add).
        const shown = kind !== 'meta' || !isPlumbing(line);
        return (
          <Fragment key={n}>
            {head ? (
              <FileHeader head={head} onOpenFile={onOpenFile} />
            ) : shown ? (
              <span className="fd-dl" data-kind={kind}>
                {numbers && kind !== 'meta' ? gutters(row, numbers.get(n)) : null}
                {line === '' ? ' ' : line}
              </span>
            ) : null}
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
