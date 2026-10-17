// DL-179: the chip of a comment short id in the chat; a click opens the thread in Changes.
import { splitTokens } from './tokens';

export function CommentChip({
  shortId,
  onOpen,
}: {
  readonly shortId: string;
  readonly onOpen?: ((shortId: string) => void) | undefined;
}) {
  if (onOpen === undefined) {
    return (
      <span className="fd-cchip" data-comment={shortId}>
        {shortId}
      </span>
    );
  }
  return (
    <button
      type="button"
      className="fd-cchip"
      data-comment={shortId}
      onClick={() => onOpen(shortId)}
    >
      {shortId}
    </button>
  );
}

/** A plain text where each known token is a chip (the prompt bubble of the chat). */
export function ChipText({
  text,
  known,
  onOpen,
}: {
  readonly text: string;
  readonly known: ReadonlySet<string>;
  readonly onOpen?: ((shortId: string) => void) | undefined;
}) {
  return (
    <>
      {splitTokens(text, known).map((piece, n) =>
        'text' in piece ? (
          piece.text
        ) : (
          <CommentChip key={n} shortId={piece.shortId} onOpen={onOpen} />
        ),
      )}
    </>
  );
}
