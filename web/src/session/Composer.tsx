import {
  type ClipboardEvent,
  type KeyboardEvent,
  useId,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from 'react';
import type { ImageInput } from '../gen';
import { useDismiss } from '../ui/dismiss';
import { ArrowUp, Icon, Paperclip } from '../ui/icons';
import type { InputKind } from './commands';
import { IMAGE_TYPES, imageFiles, imagesProblem, readImage, toDataUrl } from './images';
import { MAX_AT_OPTIONS, matchAt, MIN_AT_QUERY } from './at';
import { matchSlash } from './slash';
import { type CommentOption, hashMenu, pickComment } from '../review/hashMenu';

type Props = {
  readonly enabled: boolean;
  readonly working: boolean;
  readonly steer: boolean;
  readonly nativeQueue: boolean;
  readonly onSend: (kind: InputKind, text: string, images: readonly ImageInput[]) => void;
  /** Shown instead of the follow-up note while the composer is off. */
  readonly disabledReason?: string | null;
  /** The commands of the provider; `null` or empty: it gave no list. */
  readonly slashCommands?: readonly string[] | null;
  /** The skill names of the provider; a command in this list shows the badge "skill". */
  readonly skills?: readonly string[];
  /** The file name search of the session root (DL-174); `null` or absent: `@` has no menu. */
  readonly searchFiles?: ((query: string, maxHits: number) => Promise<readonly string[]>) | null;
  /** The comments of the session for the `#` menu (DL-179), newest first; absent or empty: no menu. */
  readonly comments?: readonly CommentOption[];
  /** With a key, the text is kept in `sessionStorage` until it is sent. */
  readonly draftKey?: string;
};

/** The most rows the text box grows to. */
const MAX_ROWS = 12;

const draftName = (key: string) => `fd-draft:${key}`;

function readDraft(key: string | undefined): string {
  if (key === undefined) return '';
  try {
    return globalThis.sessionStorage?.getItem(draftName(key)) ?? '';
  } catch {
    return '';
  }
}

function writeDraft(key: string | undefined, text: string): void {
  if (key === undefined) return;
  try {
    if (text === '') globalThis.sessionStorage?.removeItem(draftName(key));
    else globalThis.sessionStorage?.setItem(draftName(key), text);
  } catch {
    // A full or blocked storage only loses the draft.
  }
}

/** On a touch screen Enter is a newline; the send button sends. */
function coarsePointer(): boolean {
  return typeof window !== 'undefined' && window.matchMedia?.('(pointer: coarse)').matches === true;
}

/** The wait after the last key before the `@` search starts. */
const AT_DELAY_MS = 200;

/**
 * One row of a menu: `text` is the label, `badge` an optional tag. A `status` row is a note
 * (not a choice): it is disabled, and the keys skip it.
 */
export type MenuRow = {
  readonly key: string;
  readonly text: string;
  readonly badge?: string;
  readonly status?: true;
};

/** The error message that the `@` status row shows, or the fallback text. */
export function searchFailure(error: unknown): string {
  const text = error instanceof Error ? error.message : '';
  return text !== '' && text.length <= 120 ? text : 'the station did not answer';
}

/** The menu above the text box: the commands (`/`) or the file paths (`@`). */
export function ComposerMenu(props: {
  readonly id: string;
  readonly label: string;
  readonly rows: readonly MenuRow[];
  readonly current: number;
  readonly onChoose: (n: number) => void;
}) {
  return (
    <div className="fd-composer-menu" role="listbox" aria-label={props.label} id={props.id}>
      {props.rows.map((row, n) =>
        row.status ? (
          <div
            key={row.key}
            id={`${props.id}-${n}`}
            role="option"
            tabIndex={-1}
            aria-disabled="true"
            aria-selected="false"
            className="fd-composer-status"
          >
            {row.text}
          </div>
        ) : (
          <div
            key={row.key}
            id={`${props.id}-${n}`}
            role="option"
            tabIndex={-1}
            aria-selected={n === props.current}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => props.onChoose(n)}
            onKeyDown={() => {}}
          >
            {row.text}
            {row.badge ? (
              <span className="fd-badge" data-tone="muted" style={{ marginLeft: 'auto' }}>
                {row.badge}
              </span>
            ) : null}
          </div>
        ),
      )}
    </div>
  );
}

/** The message box of a session: text, slash commands, a draft and pasted images (M3 6-10, DL-140). */
export function Composer(props: Props) {
  const { draftKey } = props;
  const [text, setText] = useState(() => readDraft(draftKey));
  const [loadedKey, setLoadedKey] = useState(draftKey);
  const [images, setImages] = useState<readonly ImageInput[]>([]);
  const [readError, setReadError] = useState<string | null>(null);
  const [active, setActive] = useState(0);
  const [menuClosed, setMenuClosed] = useState(false);
  const [cursor, setCursor] = useState(0);
  const [found, setFound] = useState<{
    query: string;
    paths: readonly string[];
    failure: string | null;
  }>({ query: '', paths: [], failure: null });
  const root = useRef<HTMLFormElement>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  const listId = useId();
  const caretTo = useRef<number | null>(null);
  const calls = useRef(0);
  const search = useRef(props.searchFiles);
  search.current = props.searchFiles;
  // Another session: its draft replaces the text.
  if (loadedKey !== draftKey) {
    setLoadedKey(draftKey);
    setText(readDraft(draftKey));
  }

  const problem = readError ?? imagesProblem(images);
  const empty = text.trim() === '';
  const blocked = !props.enabled || empty || problem !== null;
  const commands = props.slashCommands ?? [];
  const hasCommands = commands.length > 0;
  const slash = hasCommands ? matchSlash(text, commands, props.skills) : null;
  const canSearch = props.searchFiles != null;
  // Precedence: the `/` menu, then the `#` menu, then the `@` menu.
  const hash = hashMenu(text, cursor, props.comments ?? [], slash !== null);
  const at = canSearch && slash === null && hash === null ? matchAt(text, cursor) : null;
  const atQuery = at !== null && at.query.length >= MIN_AT_QUERY ? at.query : null;
  const choices: MenuRow[] =
    slash !== null
      ? slash.options.map((o) => ({
          key: o.name,
          text: `/${o.name}`,
          ...(o.skill ? { badge: 'skill' } : {}),
        }))
      : hash !== null
        ? hash.rows
        : atQuery !== null
          ? found.paths.map((path) => ({ key: path, text: path }))
          : [];
  // `@` status: how many letters are missing, the search runs, it found nothing, or it failed.
  const status: string | null =
    slash !== null || at === null
      ? null
      : atQuery === null
        ? 'Type 2 or more letters to search files'
        : found.query !== atQuery
          ? 'Searching…'
          : found.failure !== null
            ? `File search failed: ${found.failure}`
            : found.paths.length === 0
              ? 'No matching files'
              : null;
  const rows: MenuRow[] =
    status === null ? choices : [...choices, { key: '\u0000status', text: status, status: true }];
  const menuOpen = props.enabled && !menuClosed && rows.length > 0;
  const current = Math.min(active, Math.max(choices.length - 1, 0));
  const optionId = (n: number) => `${listId}-${n}`;
  useDismiss(root, menuOpen, () => setMenuClosed(true));

  // `@` search: 200 ms after the last key; an answer of an older call is ignored.
  useEffect(() => {
    if (atQuery === null) return;
    calls.current += 1;
    const run = calls.current;
    const timer = setTimeout(() => {
      const call = search.current;
      if (!call) return;
      call(atQuery, MAX_AT_OPTIONS).then(
        (paths) => {
          if (calls.current === run)
            setFound({ query: atQuery, paths: paths.slice(0, MAX_AT_OPTIONS), failure: null });
        },
        (error: unknown) => {
          // A failed or refused search shows its reason in the menu.
          if (calls.current === run)
            setFound({ query: atQuery, paths: [], failure: searchFailure(error) });
        },
      );
    }, AT_DELAY_MS);
    return () => {
      clearTimeout(timer);
      calls.current += 1;
    };
  }, [atQuery]);

  // The box grows with its text, from one row to MAX_ROWS.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = 'auto';
    const style = getComputedStyle(el);
    const line = Number.parseFloat(style.lineHeight) || Number.parseFloat(style.fontSize) * 1.45;
    const extra =
      Number.parseFloat(style.paddingTop) +
      Number.parseFloat(style.paddingBottom) +
      Number.parseFloat(style.borderTopWidth) +
      Number.parseFloat(style.borderBottomWidth);
    const max = line * MAX_ROWS + (Number.isNaN(extra) ? 0 : extra);
    const wanted = el.scrollHeight + (Number.parseFloat(style.borderTopWidth) || 0) * 2;
    el.style.height = `${Math.min(wanted, max)}px`;
    el.style.overflowY = wanted > max ? 'auto' : 'hidden';
  }, [text]);

  // After a pick the caret goes behind the inserted text.
  useLayoutEffect(() => {
    const to = caretTo.current;
    if (to === null) return;
    caretTo.current = null;
    box.current?.setSelectionRange(to, to);
    setCursor(to);
  }, [text]);

  const edit = (value: string, caret = value.length) => {
    setText(value);
    setCursor(caret);
    setActive(0);
    setMenuClosed(false);
    writeDraft(draftKey, value);
  };
  const send = (kind: InputKind) => {
    const value = text.trim();
    if (value === '' || imagesProblem(images) !== null) return;
    setText('');
    setImages([]);
    setActive(0);
    setMenuClosed(false);
    writeDraft(draftKey, '');
    props.onSend(kind, value, images);
  };
  const submit = () => {
    if (!blocked) send(props.working ? 'follow_up' : 'prompt');
  };
  const choose = (n: number) => {
    const row = rows[n];
    if (row === undefined) return;
    if (slash !== null) {
      edit(`${row.text} `);
    } else if (hash !== null) {
      const picked = pickComment(text, cursor, hash.match, row.text);
      caretTo.current = picked.caret;
      edit(picked.text, picked.caret);
    } else if (at !== null) {
      const insert = `@${row.text} `;
      const next = text.slice(0, at.start) + insert + text.slice(cursor);
      caretTo.current = at.start + insert.length;
      edit(next, at.start + insert.length);
    }
    box.current?.focus();
  };
  const add = async (files: File[]) => {
    if (files.length === 0) return;
    setReadError(null);
    const read = await Promise.all(files.map((f) => readImage(f).catch(() => null)));
    const ok = read.filter((i): i is ImageInput => i !== null);
    if (ok.length < read.length) setReadError('An image could not be read.');
    setImages((now) => [...now, ...ok]);
  };
  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = imageFiles(event.clipboardData.files);
    if (files.length === 0) return;
    event.preventDefault();
    void add(files);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    const composing = event.nativeEvent.isComposing || event.keyCode === 229;
    if (composing) return;
    if (menuOpen) {
      // With status rows only, the keys keep their usual work (caret, send, focus).
      if (choices.length > 0 && event.key === 'ArrowDown') {
        event.preventDefault();
        setActive((current + 1) % choices.length);
        return;
      }
      if (choices.length > 0 && event.key === 'ArrowUp') {
        event.preventDefault();
        setActive((current - 1 + choices.length) % choices.length);
        return;
      }
      if (
        choices.length > 0 &&
        ((event.key === 'Enter' && !event.shiftKey) || event.key === 'Tab')
      ) {
        event.preventDefault();
        choose(current);
        return;
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        setMenuClosed(true);
        return;
      }
    }
    if (event.key !== 'Enter') return;
    const force = event.ctrlKey || event.metaKey;
    if (!force && (event.shiftKey || coarsePointer())) return;
    event.preventDefault();
    submit();
  };

  const reason = !props.enabled && props.disabledReason ? props.disabledReason : null;
  const placeholder = hasCommands ? 'Message the agent… (/ for commands)' : 'Message the agent…';

  return (
    <form
      ref={root}
      className="fd-composer"
      aria-label="Composer"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      {menuOpen ? (
        <ComposerMenu
          id={listId}
          label={slash !== null ? 'Commands' : hash !== null ? 'Comments' : 'Files'}
          rows={rows}
          current={current}
          onChoose={choose}
        />
      ) : null}
      <label htmlFor="fd-composer-text" className="fd-vh">
        Message
      </label>
      <textarea
        id="fd-composer-text"
        ref={box}
        rows={1}
        value={text}
        disabled={!props.enabled}
        placeholder={placeholder}
        aria-autocomplete={hasCommands || canSearch ? 'list' : undefined}
        aria-controls={menuOpen ? listId : undefined}
        aria-activedescendant={menuOpen && choices.length > 0 ? optionId(current) : undefined}
        onChange={(event) => edit(event.target.value, event.target.selectionStart)}
        onSelect={(event) => setCursor(event.currentTarget.selectionStart)}
        onKeyDown={onKeyDown}
        onPaste={onPaste}
      />
      {!hasCommands && /^\/\S*$/.test(text) ? (
        <p className="fd-composer-note">
          This session reports no commands. The text goes to the agent as a prompt.
        </p>
      ) : null}
      {images.length > 0 ? (
        <ul className="fd-images" aria-label="Images">
          {images.map((image, n) => (
            <li key={n}>
              <img src={toDataUrl(image)} alt={`Pasted ${n + 1}`} />
              <button
                type="button"
                className="fd-sm"
                aria-label={`Remove image ${n + 1}`}
                onClick={() => {
                  setReadError(null);
                  setImages((now) => now.filter((_, k) => k !== n));
                }}
              >
                Remove
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      {problem ? (
        <p role="alert" className="fd-composer-problem">
          {problem}
        </p>
      ) : null}
      <div className="fd-composer-bar">
        <label className="fd-iconbtn fd-attach" title="Add image">
          <Icon of={Paperclip} />
          <span className="fd-vh">Add image</span>
          <input
            type="file"
            accept={IMAGE_TYPES.join(',')}
            multiple
            disabled={!props.enabled}
            onChange={(event) => {
              void add(imageFiles(event.target.files ?? []));
              event.target.value = '';
            }}
          />
        </label>
        {reason ? (
          <span className="fd-composer-note">{reason}</span>
        ) : props.working && !props.nativeQueue ? (
          <span className="fd-composer-note">
            The station holds a follow-up until the turn ends.
          </span>
        ) : null}
        <div className="fd-composer-send">
          {props.working ? (
            <>
              {props.steer ? (
                <button type="button" disabled={blocked} onClick={() => send('steer')}>
                  Steer now
                </button>
              ) : null}
              <button type="submit" className="fd-primary" disabled={blocked}>
                Queue follow-up
              </button>
            </>
          ) : (
            <button type="submit" className="fd-primary" disabled={blocked}>
              <Icon of={ArrowUp} size={14} /> Send
            </button>
          )}
        </div>
      </div>
    </form>
  );
}
