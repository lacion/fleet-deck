// DL-141: the line kinds of a unified diff, for the colors of the diff view.
export type DiffLineKind = 'file' | 'meta' | 'hunk' | 'add' | 'del' | 'ctx';

const META = [
  '--- ',
  '+++ ',
  'index ',
  'new file mode',
  'deleted file mode',
  'similarity index',
  'rename from',
  'rename to',
  'Binary files',
  'old mode',
  'new mode',
];

export function diffLineKind(line: string): DiffLineKind {
  if (line.startsWith('diff --git ')) return 'file';
  if (META.some((m) => line.startsWith(m))) return 'meta';
  if (line.startsWith('@@')) return 'hunk';
  if (line.startsWith('+')) return 'add';
  if (line.startsWith('-')) return 'del';
  return 'ctx';
}
