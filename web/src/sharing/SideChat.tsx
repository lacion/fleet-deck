// R11.6 (DL-166): the "People" tab: the chat between the people of a session. The agent never
// sees it. A message is plain text, never Markdown.
import { useState } from 'react';
import type { ChatMessage } from '../gen';
import { actorLabel } from './member';
import { clock, MAX_CHAT_CHARS } from './people';

type Props = {
  readonly messages: readonly ChatMessage[];
  readonly self: string;
  readonly loading?: boolean;
  readonly error?: string | null;
  /** Sends one message; the text that comes back is an error to show. */
  readonly onSend: (text: string) => Promise<string | null>;
};

export function SideChat({ messages, self, loading = false, error = null, onSend }: Props) {
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const value = text.trim();
  const valid = value !== '' && [...value].length <= MAX_CHAT_CHARS;
  const send = () => {
    if (!valid || busy) return;
    setBusy(true);
    setProblem(null);
    void onSend(value).then((failed) => {
      setBusy(false);
      if (failed === null) setText('');
      else setProblem(failed);
    });
  };
  return (
    <section className="fd-sidechat" aria-label="People">
      <p className="fd-note">This chat is between people. The agent never sees it.</p>
      {error === null ? null : (
        <p className="fd-note fd-share-error" role="alert">
          {error}
        </p>
      )}
      {loading ? (
        <p role="status" className="fd-note">
          Loading the chat…
        </p>
      ) : messages.length === 0 ? (
        <p className="fd-empty">No messages yet.</p>
      ) : (
        <ul className="fd-sidechat-log" role="log" aria-label="Chat messages">
          {messages.map((m) => (
            <li key={m.seq} className="fd-sidechat-msg" data-own={m.author === self}>
              <span className="fd-sidechat-meta">
                <strong>{actorLabel(m.author, self)}</strong> <span>{clock(m.at)}</span>
              </span>
              <p className="fd-sidechat-text">{m.text}</p>
            </li>
          ))}
        </ul>
      )}
      <form
        className="fd-sidechat-form"
        aria-label="Write to the people"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <label className="fd-vh" htmlFor="fd-sidechat-text">
          Message to the people
        </label>
        <textarea
          id="fd-sidechat-text"
          rows={2}
          maxLength={MAX_CHAT_CHARS}
          value={text}
          placeholder="Write to the people…"
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            const composing = e.nativeEvent.isComposing || e.keyCode === 229;
            if (e.key === 'Enter' && !e.shiftKey && !composing) {
              e.preventDefault();
              send();
            }
          }}
        />
        <button type="submit" className="fd-primary" disabled={busy || !valid}>
          Send
        </button>
      </form>
      {problem === null ? null : (
        <p className="fd-note fd-share-error" role="alert">
          {problem}
        </p>
      )}
    </section>
  );
}
