// The user bubble of the chat (backlog B10): the whole prompt as plain text with its line
// breaks, folded when it is long. The fold state lives in this component only.
import { type ReactNode, useState } from 'react';
import { ChipText } from '../review/CommentChip';
import { initial } from '../sharing/people';
import { Icon, User } from '../ui/icons';

/** A prompt with more lines or characters than this is folded. */
export const FOLD_LINES = 12;
export const FOLD_CHARS = 1200;

/** The station keeps the first 32 KiB of a prompt in `run.started.input_text`; a cut ends with this mark. */
const CUT_MARK = '…';

/** The part of `text` that the folded bubble shows, or `null` when `text` is short enough. */
export function foldedText(text: string): string | null {
  const lines = text.split('\n');
  if (lines.length <= FOLD_LINES && text.length <= FOLD_CHARS) return null;
  return lines.slice(0, FOLD_LINES).join('\n').slice(0, FOLD_CHARS);
}

/**
 * The sender of a bubble (concept B): the small account avatar at its start, the person glyph of
 * the owner (the deck knows no name for its owner) or the initial of a member. It tells a short
 * prompt ("Continue") from a button. The name is in the run label under the bubble.
 */
function Sender({ author }: { readonly author: string | null }) {
  return (
    <span className="fd-avatar" data-size="sm" aria-hidden="true">
      {author === null ? <Icon of={User} size={12} /> : initial(author)}
    </span>
  );
}

/**
 * The frame of a prompt: the sender, then the text column. `sender={false}`: a prompt that no
 * person typed (the task that the main agent gave a subagent, in its log).
 */
export function Bubble(props: {
  readonly author?: string | null;
  readonly sender?: boolean;
  readonly children: ReactNode;
}) {
  return (
    <div className="fd-chat-bubble">
      {props.sender === false ? null : <Sender author={props.author ?? null} />}
      <div className="fd-chat-bubble-text">{props.children}</div>
    </div>
  );
}

export type PromptBubbleProps = {
  readonly text: string;
  /** The name of the member who typed the prompt; absent or `null` for the owner. */
  readonly author?: string | null;
  /** The comment ids that a chip may show; absent when the session has no review. */
  readonly known?: ReadonlySet<string> | undefined;
  readonly onOpen?: ((shortId: string) => void) | undefined;
  /** The fold state to start with (tests render on the server and cannot click). */
  readonly initialOpen?: boolean;
};

export function PromptBubble({
  text,
  author = null,
  known,
  onOpen,
  initialOpen = false,
}: PromptBubbleProps) {
  const [open, setOpen] = useState(initialOpen);
  const folded = foldedText(text);
  const shown = folded === null || open ? text : folded;
  return (
    <Bubble author={author}>
      {known === undefined ? shown : <ChipText text={shown} known={known} onOpen={onOpen} />}
      {folded === null ? null : (
        <button
          type="button"
          className="fd-chat-more"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
        >
          {open ? 'Show less' : 'Show more'}
        </button>
      )}
      {folded !== null && open && text.endsWith(CUT_MARK) ? (
        <p className="fd-chat-cut">The station keeps the first 32 KB of a prompt in the chat.</p>
      ) : null}
    </Bubble>
  );
}
