import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { Markdown } from './Markdown';
import { tokens } from './highlight';

const text = (parts: ReturnType<typeof tokens>) =>
  parts.map((p) => (typeof p === 'string' ? p : p[1])).join('');
const colored = (parts: ReturnType<typeof tokens>) => parts.filter((p) => typeof p !== 'string');

describe('code token colors', () => {
  test('the text never changes', () => {
    const code = 'pub fn f<\'a>(x: &\'a str) -> u8 { // note\n  let c = \'"\'; "s\\"q" }\n';
    expect(text(tokens(code, 'rust'))).toBe(code);
    const sh = "echo 'hi' # say hi\nexport A=$#\n";
    expect(text(tokens(sh, 'bash'))).toBe(sh);
  });

  test('keywords, strings and comments of a Rust block', () => {
    const parts = colored(tokens('let mask = block(); // block first\nOk("x")', 'rust'));
    expect(parts).toEqual([
      ['keyword', 'let'],
      ['comment', '// block first'],
      ['string', '"x"'],
    ]);
  });

  test('a lifetime is not a string', () => {
    const parts = colored(tokens("fn f<'a>(x: &'a str)", 'rust'));
    expect(parts).toEqual([['keyword', 'fn']]);
  });

  test('a path or method segment is not a keyword', () => {
    expect(colored(tokens('Reaper::new(x)', 'rust'))).toEqual([]);
    expect(colored(tokens('let r = Reaper::new(mask)?;', 'rust'))).toEqual([['keyword', 'let']]);
    expect(colored(tokens('opts.default; f(...this)', 'ts'))).toEqual([['keyword', 'this']]);
  });

  test('each language family has its own keywords', () => {
    // `new`, `nil`, `null` and `pass` are names in Rust.
    expect(colored(tokens('new(nil, null, pass)', 'rust'))).toEqual([]);
    expect(colored(tokens('x := nil', 'go'))).toEqual([['keyword', 'nil']]);
    expect(colored(tokens('const m = new Map()', 'ts'))).toEqual([
      ['keyword', 'const'],
      ['keyword', 'new'],
    ]);
    expect(colored(tokens('pass', 'python'))).toEqual([['keyword', 'pass']]);
    // A language without a family keeps the shared words; an Object key is not a family.
    expect(colored(tokens('new T()', 'java'))).toEqual([['keyword', 'new']]);
    expect(colored(tokens('new T()', 'constructor'))).toEqual([['keyword', 'new']]);
  });

  test('a shell block takes # comments, not // ones', () => {
    const parts = colored(tokens('ls //tmp # list\necho $#', 'sh'));
    expect(parts).toEqual([['comment', '# list']]);
  });

  test('a block without a language or with a plain one stays plain', () => {
    expect(tokens('let x = "a"', undefined)).toEqual(['let x = "a"']);
    expect(tokens('+let x', 'diff')).toEqual(['+let x']);
  });

  test('a fenced block renders the token spans and keeps its text', () => {
    const html = renderToStaticMarkup(<Markdown text={'```rust\nlet x = "a"; // c\n```'} />);
    expect(html).toContain('<code class="language-rust"><span class="fd-tok-k">let</span> x = ');
    expect(html).toContain('<span class="fd-tok-s">&quot;a&quot;</span>');
    expect(html).toContain('<span class="fd-tok-c">// c</span>');
    expect(html).toContain('<span class="fd-md-lang">rust</span>');
  });
});
