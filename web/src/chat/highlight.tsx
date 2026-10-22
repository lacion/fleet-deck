// The token colors of a fenced code block (DESIGN-SYSTEM §4.5): keywords, strings and
// comments. A small scanner, not a parser: it never changes the text, it only wraps parts of
// it in spans. A block without a language, or with a plain one, stays plain.
import type { ReactNode } from 'react';

const KEYWORDS = new Set(
  // Rust, TypeScript and JavaScript, Python, Go, shell: the words that read as keywords in all.
  (
    'as async await break case catch class const continue def default defer del do done dyn elif ' +
    'else enum esac except export extends extern false fi finally fn for from func function go if ' +
    'impl import in interface is lambda let loop match mod move mut new nil null package pass pub ' +
    'raise return self static struct super switch then this throw trait true try type typeof ' +
    'undefined unsafe use var where while with yield None True False Self'
  ).split(' '),
);

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
  for (let m = re.exec(code); m !== null; m = re.exec(code)) {
    const [text, comment, string] = m;
    const kind: TokenKind | null =
      comment !== undefined
        ? 'comment'
        : string !== undefined
          ? 'string'
          : KEYWORDS.has(text)
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
