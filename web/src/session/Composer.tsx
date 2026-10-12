import {
  type ClipboardEvent,
  type KeyboardEvent,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from 'react';
import type { ImageInput } from '../gen';
import { ArrowUp, Icon, Paperclip } from '../ui/icons';
import type { InputKind } from './commands';
import { IMAGE_TYPES, imageFiles, imagesProblem, readImage, toDataUrl } from './images';
import { matchSlash } from './slash';

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

/** The message box of a session: text, slash commands, a draft and pasted images (M3 6-10, DL-140). */
export function Composer(props: Props) {
  const { draftKey } = props;
  const [text, setText] = useState(() => readDraft(draftKey));
  const [loadedKey, setLoadedKey] = useState(draftKey);
  const [images, setImages] = useState<readonly ImageInput[]>([]);
  const [readError, setReadError] = useState<string | null>(null);
  const [active, setActive] = useState(0);
  const [menuClosed, setMenuClosed] = useState(false);
  const box = useRef<HTMLTextAreaElement>(null);
  const listId = useId();
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
  const slash = hasCommands ? matchSlash(text, commands) : null;
  const options = slash?.options ?? [];
  const menuOpen = props.enabled && !menuClosed && options.length > 0;
  const current = Math.min(active, Math.max(options.length - 1, 0));
  const optionId = (n: number) => `${listId}-${n}`;

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

  const edit = (value: string) => {
    setText(value);
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
  const choose = (command: string) => {
    edit(`/${command} `);
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
      if (event.key === 'ArrowDown') {
        event.preventDefault();
        setActive((current + 1) % options.length);
        return;
      }
      if (event.key === 'ArrowUp') {
        event.preventDefault();
        setActive((current - 1 + options.length) % options.length);
        return;
      }
      if ((event.key === 'Enter' && !event.shiftKey) || event.key === 'Tab') {
        const command = options[current];
        if (command !== undefined) {
          event.preventDefault();
          choose(command);
          return;
        }
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
      className="fd-composer"
      aria-label="Composer"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      {menuOpen ? (
        <div className="fd-composer-menu" role="listbox" aria-label="Commands" id={listId}>
          {options.map((command, n) => (
            <div
              key={command}
              id={optionId(n)}
              role="option"
              tabIndex={-1}
              aria-selected={n === current}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => choose(command)}
              onKeyDown={() => {}}
            >
              /{command}
            </div>
          ))}
        </div>
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
        aria-autocomplete={hasCommands ? 'list' : undefined}
        aria-controls={menuOpen ? listId : undefined}
        aria-activedescendant={menuOpen ? optionId(current) : undefined}
        onChange={(event) => edit(event.target.value)}
        onKeyDown={onKeyDown}
        onPaste={onPaste}
      />
      {!hasCommands && text.startsWith('/') ? (
        <p className="fd-composer-note">
          This provider gave no command list. The text goes to the agent as a prompt.
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
