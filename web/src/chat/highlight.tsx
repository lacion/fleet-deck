// The token colors of a fenced code block (DESIGN-SYSTEM §4.5): keywords, strings and
// comments. A small scanner, not a parser: it never changes the text, it only wraps parts of
// it in spans. A block without a language, or with a plain one, stays plain.
import type { ReactNode } from 'react';

/** A set of the words of a space-separated list. */
const words = (list: string): ReadonlySet<string> => new Set(list.split(' '));

// The keywords of each language family: a word of one language (`new`, `nil`, `pass`) is a plain
// name in another (`Reaper::new`).
const RUST = words(
  'as async await break const continue crate dyn else enum extern false fn for if impl in let ' +
    'loop match mod move mut pub ref return self Self static struct super trait true type unsafe ' +
    'use where while',
);
const JS = words(
  'abstract as async await break case catch class const continue debugger declare default ' +
    'delete do else enum export extends false finally for from function if implements import in ' +
    'instanceof interface keyof let new null of package private protected public readonly return ' +
    'satisfies static super switch this throw true try type typeof undefined var void while with ' +
    'yield',
);
const PYTHON = words(
  'and as assert async await break case class continue def del elif else except False finally ' +
    'for from global if import in is lambda match None nonlocal not or pass raise return self ' +
    'True try while with yield',
);
const GO = words(
  'break case chan const continue default defer else fallthrough false for func go goto if ' +
    'import interface iota map nil package range return select struct switch true type var',
);
const SHELL = words(
  'break case continue declare do done elif else esac export fi for function if in local ' +
    'readonly return select then unset until while',
);
const JSON_WORDS = words('false null true');
/** Any other language: the words that read as keywords in most of them. */
const OTHER = words(
  'as async await break case catch class const continue def default defer del do done dyn elif ' +
    'else enum esac except export extends extern false fi finally fn for from func function go if ' +
    'impl import in interface is lambda let loop match mod move mut new nil null package pass pub ' +
    'raise return self static struct super switch then this throw trait true try type typeof ' +
    'undefined unsafe use var where while with yield None True False Self',
);

/** The keyword set of each language name (a Map: a name like `constructor` is not a key). */
const FAMILY = new Map<string, ReadonlySet<string>>(
  (
    [
      [RUST, 'rust rs'],
      [JS, 'js jsx mjs cjs javascript ts tsx mts cts typescript'],
      [PYTHON, 'py python'],
      [GO, 'go golang'],
      [SHELL, 'sh bash shell zsh fish console'],
      [JSON_WORDS, 'json jsonc'],
    ] as const
  ).flatMap(([set, names]) => names.split(' ').map((name) => [name, set] as const)),
);

/** A word right after `.` or `::` is a field, a method or a path segment, never a keyword
    (`Reaper::new`, `opts.default`); a spread (`...this`) is not a field. */
function isMember(code: string, at: number): boolean {
  if (code[at - 1] === '.') return code.slice(Math.max(0, at - 3), at) !== '...';
  return code.slice(Math.max(0, at - 2), at) === '::';
}

/** Languages with `#` comments. */
const HASH = new Set([
  'sh',
  'bash',
  'shell',
  'zsh',
  'fish',
  'console',
  'py',
  'python',
  'rb',
  'ruby',
  'yaml',
  'yml',
  'toml',
  'ini',
  'conf',
  'dockerfile',
  'make',
  'makefile',
  'nix',
  'r',
  'perl',
]);

/** Languages where a single quote is a character (or a lifetime), not a string. */
const CHAR_QUOTE = new Set(['rust', 'rs', 'c', 'h', 'cpp', 'hpp', 'go', 'java', 'kotlin', 'kt']);

/** Languages whose text has no tokens to color. */
const PLAIN = new Set([
  'text',
  'txt',
  'plain',
  'plaintext',
  'diff',
  'patch',
  'md',
  'markdown',
  'log',
]);

export type TokenKind = 'keyword' | 'string' | 'comment';

const DOUBLE = String.raw`"(?:\\.|[^"\\\n])*"`;
const SINGLE = String.raw`'(?:\\.|[^'\\\n])*'`;
const CHAR = String.raw`'(?:\\.|[^'\\\n])'`;
const BACKTICK = String.raw`\`(?:\\.|[^\`\\])*\``;
const WORD = String.raw`\b[A-Za-z_][A-Za-z0-9_]*\b`;

function scanner(lang: string): RegExp {
  const comment = HASH.has(lang)
    ? String.raw`(?<![^\s])#[^\n]*`
    : lang === 'sql' || lang === 'lua'
      ? String.raw`--[^\n]*`
      : String.raw`\/\/[^\n]*|\/\*[\s\S]*?\*\/`;
  const strings = [DOUBLE, CHAR_QUOTE.has(lang) ? CHAR : SINGLE, BACKTICK].join('|');
  return new RegExp(`(${comment})|(${strings})|(${WORD})`, 'g');
}

/** The parts of `code`: plain strings and colored tokens, in order; their text is `code`. */
export function tokens(code: string, lang: string | undefined): (string | [TokenKind, string])[] {
  const name = lang?.toLowerCase();
  if (name === undefined || name === '' || PLAIN.has(name)) return code === '' ? [] : [code];
  const out: (string | [TokenKind, string])[] = [];
  let plain = '';
  let at = 0;
  const re = scanner(name);
  const keywords = FAMILY.get(name) ?? OTHER;
  for (let m = re.exec(code); m !== null; m = re.exec(code)) {
    const [text, comment, string] = m;
    const kind: TokenKind | null =
      comment !== undefined
        ? 'comment'
        : string !== undefined
          ? 'string'
          : keywords.has(text) && !isMember(code, m.index)
            ? 'keyword'
            : null;
    if (kind === null) continue;
    plain += code.slice(at, m.index);
    if (plain !== '') out.push(plain);
    plain = '';
    out.push([kind, text]);
    at = m.index + text.length;
  }
  plain += code.slice(at);
  if (plain !== '') out.push(plain);
  return out;
}

const CLASS: Record<TokenKind, string> = {
  keyword: 'fd-tok-k',
  string: 'fd-tok-s',
  comment: 'fd-tok-c',
};

/** The code with its token spans. */
export function highlight(code: string, lang: string | undefined): ReactNode[] {
  return tokens(code, lang).map((part, i) =>
    typeof part === 'string' ? (
      part
    ) : (
      <span key={i} className={CLASS[part[0]]}>
        {part[1]}
      </span>
    ),
  );
}
