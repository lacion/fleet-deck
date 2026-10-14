// DL-179: the comment tokens (`c-xxxxxx` and `[c-xxxxxx]`) of the chat. A token becomes a chip
// only when its id is the full short id of a comment that the caller may see.

/**
 * A comment short id (`c-` and 6 to 12 characters of the deck alphabet, without `l`, `o`, `0`,
 * `1`), with optional square brackets, as a whole word.
 */
export const COMMENT_TOKEN = /(?<![A-Za-z0-9_-])\[?(c-[a-km-np-z2-9]{6,12})\]?(?![A-Za-z0-9_-])/g;

export type Piece = { readonly text: string } | { readonly shortId: string; readonly raw: string };

/** `text` cut into text pieces and chip pieces; a match is a chip only when its id is in `known`. */
export function splitTokens(text: string, known: ReadonlySet<string>): Piece[] {
  const pieces: Piece[] = [];
  const addText = (value: string) => {
    if (value === '') return;
    const last = pieces.at(-1);
    if (last !== undefined && 'text' in last)
      pieces[pieces.length - 1] = { text: last.text + value };
    else pieces.push({ text: value });
  };
  let at = 0;
  for (const match of text.matchAll(COMMENT_TOKEN)) {
    const id = match[1];
    const start = match.index ?? 0;
    if (id === undefined || !known.has(id)) continue;
    addText(text.slice(at, start));
    pieces.push({ shortId: id, raw: match[0] });
    at = start + match[0].length;
  }
  addText(text.slice(at));
  return pieces;
}

type MdNode = {
  type: string;
  value?: string;
  children?: MdNode[];
  data?: Record<string, unknown>;
};

/** The mdast nodes that never hold a chip: a link, a link reference and code. */
const SKIPPED = new Set(['link', 'linkReference', 'inlineCode', 'code']);

/**
 * The element of a chip. A `strong` node whose `hName` is `span` renders `<span
 * data-comment="c-…">raw</span>` in react-markdown (a `text` node would ignore `hName`).
 */
const chipNode = (shortId: string, raw: string): MdNode => ({
  type: 'strong',
  data: { hName: 'span', hProperties: { dataComment: shortId } },
  children: [{ type: 'text', value: raw }],
});

function walk(parent: MdNode, known: ReadonlySet<string>): void {
  const children = parent.children;
  if (children === undefined) return;
  const next: MdNode[] = [];
  for (const child of children) {
    if (child.type === 'text' && typeof child.value === 'string') {
      const pieces = splitTokens(child.value, known);
      if (pieces.every((p) => 'text' in p)) {
        next.push(child);
        continue;
      }
      for (const p of pieces) {
        next.push('text' in p ? { type: 'text', value: p.text } : chipNode(p.shortId, p.raw));
      }
      continue;
    }
    if (!SKIPPED.has(child.type)) walk(child, known);
    next.push(child);
  }
  parent.children = next;
}

/**
 * A remark plugin: the tokens of `known` comments in the text of a Markdown document become
 * `<span data-comment>` elements, except inside a link, a link reference, inline code or a code
 * block.
 */
export function remarkCommentChips(known: ReadonlySet<string>) {
  return () => (tree: MdNode) => {
    walk(tree, known);
  };
}
