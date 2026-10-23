// The number columns of a Markdown table in the chat (DESIGN-SYSTEM §2.3: a column of numbers
// aligns right). A remark plugin marks every cell of such a column with `data-num`; chat.css
// aligns it.

/** A number, with an optional sign, currency, percent or short unit: `779`, `−3.5`, `55%`, `16 h`, `$1.20`, `9.4K`, `1.2 GB`. */
const NUMBER = /^[−+-]?[$€£]?\d[\d.,]*\s?(?:%|[A-Za-z]{1,3})?$/;

/** A cell that says nothing (empty, or a dash): it neither makes nor breaks a number column. */
const BLANK = /^[—–-]?$/;

/** True for the text of a cell that is a number. */
export const isNumberCell = (text: string): boolean => NUMBER.test(text.trim());

type MdNode = {
  type: string;
  value?: string;
  align?: (string | null)[] | null;
  children?: MdNode[];
  data?: { hProperties?: Record<string, unknown> } & Record<string, unknown>;
};

/** The plain text of a cell (its text and inline code). */
function textOf(node: MdNode): string {
  if (typeof node.value === 'string') return node.value;
  return (node.children ?? []).map(textOf).join('');
}

/** The indexes of the columns of `table` whose body cells are numbers (and at least one is). */
function numberColumns(table: MdNode): Set<number> {
  const rows = table.children ?? [];
  const columns = new Set<number>();
  const width = Math.max(0, ...rows.map((row) => row.children?.length ?? 0));
  for (let col = 0; col < width; col++) {
    // A column that the author aligned keeps that alignment.
    if (table.align?.[col] != null) continue;
    let numbers = 0;
    let words = 0;
    for (const row of rows.slice(1)) {
      const cell = row.children?.[col];
      const text = cell === undefined ? '' : textOf(cell).trim();
      if (BLANK.test(text)) continue;
      if (isNumberCell(text)) numbers += 1;
      else words += 1;
    }
    if (numbers > 0 && words === 0) columns.add(col);
  }
  return columns;
}

function walk(node: MdNode): void {
  if (node.type === 'table') {
    const columns = numberColumns(node);
    if (columns.size === 0) return;
    for (const row of node.children ?? []) {
      (row.children ?? []).forEach((cell, col) => {
        if (!columns.has(col)) return;
        const data = cell.data ?? {};
        cell.data = { ...data, hProperties: { ...data.hProperties, dataNum: '' } };
      });
    }
    return;
  }
  for (const child of node.children ?? []) walk(child);
}

/** A remark plugin: every cell of a number column (its header too) gets `data-num`. */
export function remarkNumberColumns() {
  return (tree: MdNode) => {
    walk(tree);
  };
}
