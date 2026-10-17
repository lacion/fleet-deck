// R11.4 (DL-166): the page of an invite link. It comes before the login page.
import { useState } from 'react';
import { displayName, type JoinResult, MAX_DISPLAY_NAME_CHARS } from './join';

type Props = {
  /** Redeems the invite with the display name. */
  readonly onJoin: (name: string) => Promise<JoinResult>;
  /** The join worked: the board replaces the hash and loads the connection again. */
  readonly onJoined: (result: Extract<JoinResult, { ok: true }>) => void;
  /** An error text, so that a static render shows it (tests). */
  readonly initialError?: string;
  readonly initialName?: string;
  readonly initialBusy?: boolean;
};

export function JoinPage({
  onJoin,
  onJoined,
  initialError,
  initialName = '',
  initialBusy = false,
}: Props) {
  const [name, setName] = useState(initialName);
  const [busy, setBusy] = useState(initialBusy);
  const [error, setError] = useState<string | null>(initialError ?? null);
  const valid = displayName(name) !== null;
  return (
    <main className="fd-login fd-join">
      <h1>Join a shared Fleet Deck</h1>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          const value = displayName(name);
          if (value === null) return;
          setBusy(true);
          setError(null);
          void onJoin(value).then((result) => {
            if (result.ok) onJoined(result);
            else {
              setError(result.text);
              setBusy(false);
            }
          });
        }}
      >
        <label htmlFor="join-name">Display name</label>
        <input
          id="join-name"
          type="text"
          autoComplete="nickname"
          required
          maxLength={MAX_DISPLAY_NAME_CHARS}
          value={name}
          onChange={(event) => setName(event.target.value)}
        />
        <button type="submit" disabled={busy || !valid}>
          {busy ? 'Joining…' : 'Join'}
        </button>
      </form>
      <p className="fd-note">
        You join as a guest of the owner's deck. The owner can end your access at any time.
      </p>
      {error === null ? null : <p role="alert">{error}</p>}
    </main>
  );
}
