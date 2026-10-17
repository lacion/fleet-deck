// B7: the dialog that lists the native sessions of a station, shows a preview of one, and
// continues it in the New session form (design native-sessions.md §2.6).
import { useEffect, useRef, useState } from 'react';
import { trapTab, useFocusReturn } from '../app/focus';
import { useOverlay } from '../app/overlay';
import { formatHash } from '../app/routes';
import { HistoryItemView } from '../chat/HistoryBlock';
import { historyStateLine } from '../chat/history';
import type { NativePreview, NativeSession, NativeSessionList, StationProvider } from '../gen';
import { Icon, X } from '../ui/icons';
import {
  ageText,
  continueFill,
  entryInstance,
  entryTitle,
  groupNative,
  importStateLine,
  listNotes,
  type NativeActions,
  type NativeError,
  type NativeFill,
  type NativeRow,
  nativeError,
  previewQuery,
  sizeText,
  unverifiedNote,
} from './nativeSessions';

type Props = {
  readonly stationId: string;
  readonly stationName: string;
  readonly providers: readonly StationProvider[];
  readonly actions: NativeActions;
  readonly onClose: () => void;
  /** Fills the New session form; the dialog closes after it. */
  readonly onContinue: (fill: NativeFill) => void;
  /** Unix ms of now for the ages (tests). */
  readonly now?: number;
  /** The first list, so that a static render shows a state (tests). Given: no first load. */
  readonly initialList?: NativeSessionList;
  /** The entry that is open at the first render (tests). */
  readonly initialChosen?: NativeSession;
  /** The preview at the first render (tests). */
  readonly initialPreview?: NativePreview;
};

type ListState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly error: NativeError }
  | { readonly kind: 'done'; readonly list: NativeSessionList };

type PreviewState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly error: NativeError }
  | { readonly kind: 'done'; readonly preview: NativePreview };

function sessionHref(stationId: string, sessionId: string): string {
  return formatHash({ name: 'session', station: stationId, session: sessionId, tab: 'chat' });
}

/** One entry of the list: a button that opens the preview, then its muted lines. */
export function NativeRowView({
  row,
  stationId,
  now,
  onChoose,
  onNavigate,
}: {
  readonly row: NativeRow;
  readonly stationId: string;
  readonly now: number;
  readonly onChoose: (s: NativeSession) => void;
  readonly onNavigate: () => void;
}) {
  const s = row.session;
  const state = importStateLine(s.import);
  const note = unverifiedNote(s);
  const known = s.known;
  const open = (id: string) => (
    <a href={sessionHref(stationId, id)} onClick={onNavigate}>
      Open it
    </a>
  );
  return (
    <li className="fd-native-row" data-depth={row.depth} data-import={s.import.state}>
      <button type="button" className="fd-native-open" onClick={() => onChoose(s)}>
        <span className="fd-native-title">{entryTitle(s)}</span>
        {s.cwd === null ? null : <span className="fd-native-cwd">{s.cwd}</span>}
        <span className="fd-native-meta" title={new Date(s.modified_at).toLocaleString()}>
          {ageText(s.modified_at, now)} · {sizeText(s.size)}
        </span>
      </button>
      {known === null ? null : (
        <p className="fd-note fd-native-line">
          Already in Fleet Deck {state?.link === known.session_id ? null : open(known.session_id)}
        </p>
      )}
      {state === null ? null : (
        <p className="fd-note fd-native-line">
          {state.text} {state.link === null ? null : open(state.link)}
        </p>
      )}
      {note === null ? null : <p className="fd-note fd-native-line">{note}</p>}
    </li>
  );
}

/** The list, grouped by provider instance. */
export function NativeListView({
  list,
  stationId,
  providers,
  now,
  onChoose,
  onNavigate,
}: {
  readonly list: NativeSessionList;
  readonly stationId: string;
  readonly providers: readonly StationProvider[];
  readonly now: number;
  readonly onChoose: (s: NativeSession) => void;
  readonly onNavigate: () => void;
}) {
  const groups = groupNative(list, providers);
  const notes = listNotes(list);
  return (
    <>
      {groups.length === 0 ? <p className="fd-note">No sessions on this station.</p> : null}
      {groups.map((g) => (
        <section key={g.instance} className="fd-native-group" aria-label={g.heading}>
          <h3 className="fd-label">{g.heading}</h3>
          {g.unavailable.map((line) => (
            <p key={line} className="fd-note">
              {line}
            </p>
          ))}
          {g.rows.length === 0 ? null : (
            <ul className="fd-native-list">
              {g.rows.map((r) => (
                <NativeRowView
                  key={r.session.native_id}
                  row={r}
                  stationId={stationId}
                  now={now}
                  onChoose={onChoose}
                  onNavigate={onNavigate}
                />
              ))}
            </ul>
          )}
        </section>
      ))}
      {notes.map((n) => (
        <p key={n} className="fd-note">
          {n}
        </p>
      ))}
    </>
  );
}

/** The preview of one entry, with "Continue it". */
export function NativePreviewView({
  session,
  state,
  fill,
  onBack,
  onContinue,
  onRetry,
}: {
  readonly session: NativeSession;
  readonly state: PreviewState;
  /** `null`: the entry cannot be continued. */
  readonly fill: NativeFill | null;
  readonly onBack: () => void;
  readonly onContinue: () => void;
  readonly onRetry: () => void;
}) {
  const reason = importStateLine(session.import);
  const note = unverifiedNote(session);
  const line = state.kind === 'done' ? historyStateLine(state.preview.state) : null;
  return (
    <div className="fd-native-preview">
      <div className="fd-native-preview-head">
        <button type="button" className="fd-ghost fd-sm" onClick={onBack}>
          Back to the list
        </button>
        <p className="fd-native-title">{entryTitle(session)}</p>
        {session.cwd === null ? null : <p className="fd-native-cwd">{session.cwd}</p>}
      </div>
      {state.kind === 'loading' ? (
        <p className="fd-note" role="status">
          Reading the conversation…
        </p>
      ) : null}
      {state.kind === 'error' ? (
        <div>
          <p role="alert">{state.error.text}</p>
          {state.error.retry ? (
            <button type="button" className="fd-sm" onClick={onRetry}>
              Try again
            </button>
          ) : null}
        </div>
      ) : null}
      {state.kind === 'done' ? (
        <div className="fd-native-items">
          {state.preview.earlier_omitted ? (
            <p className="fd-note">Earlier messages are not shown.</p>
          ) : null}
          {state.preview.items.map((item) => (
            <HistoryItemView key={item.id} item={item} />
          ))}
          {line === null ? null : <p className="fd-note">{line.text}</p>}
        </div>
      ) : null}
      {reason === null ? null : <p className="fd-note">{reason.text}</p>}
      {note === null ? null : <p className="fd-note">{note}</p>}
      <div className="fd-native-foot">
        <button type="button" className="fd-primary" disabled={fill === null} onClick={onContinue}>
          Continue it
        </button>
      </div>
    </div>
  );
}

/** The list of a station and the preview of one entry in a dialog. */
export function BrowseSessions({
  stationId,
  stationName,
  providers,
  actions,
  onClose,
  onContinue,
  now = Date.now(),
  initialList,
  initialChosen,
  initialPreview,
}: Props) {
  const root = useRef<HTMLDivElement>(null);
  const [listState, setListState] = useState<ListState>(
    initialList === undefined ? { kind: 'loading' } : { kind: 'done', list: initialList },
  );
  const [chosen, setChosen] = useState<NativeSession | null>(initialChosen ?? null);
  const [preview, setPreview] = useState<PreviewState>(
    initialPreview === undefined ? { kind: 'loading' } : { kind: 'done', preview: initialPreview },
  );
  // A late answer of an older request is dropped.
  const token = useRef(0);
  useFocusReturn(root);
  useOverlay();

  const loadList = () => {
    token.current += 1;
    const mine = token.current;
    setListState({ kind: 'loading' });
    actions.list(stationId).then(
      (list) => {
        if (token.current === mine) setListState({ kind: 'done', list });
      },
      (error: unknown) => {
        if (token.current === mine) setListState({ kind: 'error', error: nativeError(error) });
      },
    );
  };
  const loadPreview = (s: NativeSession) => {
    token.current += 1;
    const mine = token.current;
    setPreview({ kind: 'loading' });
    actions.preview(stationId, previewQuery(s, entryInstance(s, providers))).then(
      (p) => {
        if (token.current === mine) setPreview({ kind: 'done', preview: p });
      },
      (error: unknown) => {
        if (token.current === mine) setPreview({ kind: 'error', error: nativeError(error) });
      },
    );
  };
  useEffect(() => {
    if (initialList === undefined) loadList();
    return () => {
      token.current += 1;
    };
  }, []);

  // The button that was pressed goes away with its view: the focus moves to the first button of the
  // new view ("Back to the list", or the first entry), so that Escape and Tab keep working.
  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    root.current?.querySelector('button')?.focus();
  }, [chosen]);

  const choose = (s: NativeSession) => {
    setChosen(s);
    loadPreview(s);
  };
  const back = () => {
    token.current += 1;
    setChosen(null);
  };
  const title = `Sessions on ${stationName}`;
  return (
    // The overlay click is a mouse shortcut; the Close button and Escape serve the keyboard.
    <div
      className="fd-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={root}
        className="fd-dialog fd-native"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            if (chosen === null) onClose();
            else back();
          } else trapTab(e, root.current);
        }}
      >
        <h2>{title}</h2>
        {chosen !== null ? (
          <NativePreviewView
            session={chosen}
            state={preview}
            fill={continueFill(chosen, providers)}
            onBack={back}
            onRetry={() => loadPreview(chosen)}
            onContinue={() => {
              const fill = continueFill(chosen, providers);
              if (fill === null) return;
              onContinue(fill);
              onClose();
            }}
          />
        ) : listState.kind === 'loading' ? (
          <p className="fd-note" role="status">
            Reading the sessions…
          </p>
        ) : listState.kind === 'error' ? (
          <div>
            <p role="alert">{listState.error.text}</p>
            {listState.error.retry ? (
              <button type="button" className="fd-sm" onClick={loadList}>
                Try again
              </button>
            ) : null}
          </div>
        ) : (
          <NativeListView
            list={listState.list}
            stationId={stationId}
            providers={providers}
            now={now}
            onChoose={choose}
            onNavigate={onClose}
          />
        )}
        <div className="fd-exec-foot">
          <button type="button" className="fd-ghost" onClick={onClose}>
            <Icon of={X} size={14} /> Close
          </button>
        </div>
      </div>
    </div>
  );
}
