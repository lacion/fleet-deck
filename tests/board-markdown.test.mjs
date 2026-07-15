// tests/board-markdown.test.mjs
//
// Pure tests for the board's local markdown renderer (board/src/markdown.js).
//
// This renderer is the board's XSS wall: plan/mail text is agent-authored and
// therefore untrusted, and renderMarkdown()'s output is inserted as HTML. The
// module's whole contract is "HTML-escape & < > \" ' BEFORE a single tag is
// added, and support NO link/image/href syntax", so a hostile plan can never
// inject markup. These tests pin that invariant along with the ordinary
// structural rendering (fences, headings, bold, bullets).
//
// board/src/markdown.js has no imports and board/package.json is "type":
// "module", so it loads under `node --test` with no bundler — same as
// tests/board-util.test.mjs.

import test from 'node:test';
import assert from 'node:assert/strict';

import { renderMarkdown, planTitle } from '../board/src/markdown.js';

// The five characters the renderer must neutralise, and their escapes.
const ESCAPES = {
  '&': '&amp;',
  '<': '&lt;',
  '>': '&gt;',
  '"': '&quot;',
  "'": '&#39;',
};

// -------------------------------------------------------- the security invariant

test('a <script> payload is escaped, never emitted raw', () => {
  const out = renderMarkdown('<script>alert(1)</script>');
  assert.ok(!out.includes('<script>'), 'raw <script> tag must not survive');
  assert.ok(!out.includes('</script>'), 'raw </script> tag must not survive');
  assert.ok(out.includes('&lt;script&gt;'), 'the payload must appear escaped');
  assert.ok(out.includes('&lt;/script&gt;'));
});

test('every dangerous char is escaped in plain paragraph text', () => {
  const out = renderMarkdown(`amp & lt < gt > quot " apos '`);
  for (const [raw, esc] of Object.entries(ESCAPES)) {
    assert.ok(out.includes(esc), `expected ${esc} in output`);
  }
  // and no bare < / > / " / ' leaked as structural characters beyond the
  // <p>...</p> wrapper the renderer itself adds
  const inner = out.replace(/^<p>/, '').replace(/<\/p>$/, '');
  assert.ok(!inner.includes('<'), 'no raw < inside the paragraph body');
  assert.ok(!inner.includes('>'), 'no raw > inside the paragraph body');
  assert.ok(!inner.includes('"'), 'no raw " inside the paragraph body');
  assert.ok(!inner.includes("'"), "no raw ' inside the paragraph body");
});

test('escaping happens INSIDE code spans', () => {
  const out = renderMarkdown('`<b>&"\'`');
  assert.ok(out.includes('<code>'), 'a code span should still be emitted');
  assert.ok(out.includes('&lt;b&gt;&amp;&quot;&#39;'), 'code span content is escaped');
  assert.ok(!out.includes('<b>'), 'no raw markup smuggled through a code span');
});

test('escaping happens INSIDE fenced code blocks', () => {
  const out = renderMarkdown('```\n<script>x</script> & "\'\n```');
  assert.ok(out.includes('<pre><code>'), 'a fence should render to <pre><code>');
  assert.ok(out.includes('&lt;script&gt;x&lt;/script&gt;'), 'fence content is escaped');
  assert.ok(out.includes('&amp;'));
  assert.ok(out.includes('&quot;'));
  assert.ok(out.includes('&#39;'));
  assert.ok(!out.includes('<script>'), 'no raw script through a fence');
});

test('escaping happens INSIDE bold/strong', () => {
  const out = renderMarkdown('**<i>&</i>**');
  assert.ok(out.includes('<strong>'), 'bold should render to <strong>');
  assert.ok(out.includes('&lt;i&gt;&amp;&lt;/i&gt;'), 'strong content is escaped');
  assert.ok(!out.includes('<i>'), 'no raw <i> smuggled through bold');
});

test('escaping happens INSIDE headings', () => {
  const out = renderMarkdown('# <h1>title & "quoted"');
  // exactly one real <h1> wrapper — the injected one is escaped, not doubled
  assert.equal((out.match(/<h1>/g) || []).length, 1, 'only the renderer\'s own <h1> tag');
  assert.ok(out.includes('&lt;h1&gt;'), 'the injected <h1> is escaped');
  assert.ok(out.includes('&amp;'));
  assert.ok(out.includes('&quot;quoted&quot;'));
});

test('escaping is applied BEFORE tag-wrapping: a payload cannot break out of its own tag', () => {
  // A naive renderer that wraps first and escapes second would let the injected
  // </strong> close the tag early and drop a live <script> after it.
  const out = renderMarkdown('**</strong><script>evil</script>**');
  // the ONLY </strong> is the renderer's matching close tag; the injected one
  // must be escaped
  assert.equal((out.match(/<\/strong>/g) || []).length, 1);
  assert.ok(out.includes('&lt;/strong&gt;'), 'the injected close tag is escaped');
  assert.ok(!out.includes('<script>'), 'no live script escapes the strong span');
  assert.ok(out.includes('&lt;script&gt;evil&lt;/script&gt;'));
});

// ------------------------------------------- no link/image/href attribute vector

test('link/image syntax is NOT honoured — no href, src, <a>, or <img> ever', () => {
  const out = renderMarkdown(
    '[click me](javascript:alert(1)) then ![pic](http://evil.example/x.png "t")',
  );
  assert.ok(!out.includes('href='), 'renderer must never emit an href attribute');
  assert.ok(!out.includes('src='), 'renderer must never emit a src attribute');
  assert.ok(!out.includes('<a '), 'renderer must never emit an anchor');
  assert.ok(!out.includes('<a>'));
  assert.ok(!out.includes('<img'), 'renderer must never emit an image');
  // the bracket/paren characters are not escaped and carry no meaning, so the
  // whole thing renders as literal text inside a paragraph
  assert.ok(out.includes('[click me](javascript:alert(1))'), 'link syntax is literal text');
  assert.ok(out.includes('![pic](http://evil.example/x.png'), 'image syntax is literal text');
});

test('raw HTML anchors / images / autolinks are escaped to inert text, never live tags', () => {
  // These carry real HTML. The renderer neutralises them by escaping the angle
  // brackets, so an attribute substring like `href=` may survive as inert TEXT
  // (its `<` is escaped, so nothing renders as a tag). The invariant that
  // matters is: no LIVE tag survives.
  const cases = [
    ['<https://evil.example>', '&lt;https://evil.example&gt;'],
    ['<a href="javascript:alert(1)">x</a>', '&lt;a href=&quot;javascript:alert(1)&quot;&gt;'],
    ['<img src=x onerror=alert(1)>', '&lt;img src=x onerror=alert(1)&gt;'],
  ];
  for (const [md, escaped] of cases) {
    const out = renderMarkdown(md);
    assert.ok(!out.includes('<a '), `live anchor leaked for: ${md}`);
    assert.ok(!out.includes('<a>'), `live anchor leaked for: ${md}`);
    assert.ok(!out.includes('<img'), `live image leaked for: ${md}`);
    assert.ok(!out.includes('<script'), `live script leaked for: ${md}`);
    assert.ok(out.includes(escaped), `the raw HTML must be present but escaped: ${md}`);
  }
});

// ----------------------------------------------- structure still renders (escaped)

test('headings render to <h1>/<h2>/<h3> by depth', () => {
  assert.ok(renderMarkdown('# one').includes('<h1>one</h1>'));
  assert.ok(renderMarkdown('## two').includes('<h2>two</h2>'));
  assert.ok(renderMarkdown('### three').includes('<h3>three</h3>'));
});

test('bold renders to <strong>, code span to <code>, fence to <pre><code>', () => {
  assert.ok(renderMarkdown('**bold**').includes('<strong>bold</strong>'));
  assert.ok(renderMarkdown('`snippet`').includes('<code>snippet</code>'));
  assert.equal(renderMarkdown('```\nplain\n```'), '<pre><code>plain</code></pre>');
});

test('bullet lists render to <ul><li>...', () => {
  const out = renderMarkdown('- alpha\n- beta');
  assert.ok(out.startsWith('<ul>'));
  assert.ok(out.endsWith('</ul>'));
  assert.ok(out.includes('<li>alpha</li>'));
  assert.ok(out.includes('<li>beta</li>'));
});

test('a code span wins over bold, so ** inside backticks never bolds', () => {
  const out = renderMarkdown('`a ** b`');
  assert.ok(out.includes('<code>a ** b</code>'));
  assert.ok(!out.includes('<strong>'));
});

test('empty / nullish input renders to the empty string, never throwing', () => {
  assert.equal(renderMarkdown(''), '');
  assert.equal(renderMarkdown(null), '');
  assert.equal(renderMarkdown(undefined), '');
  assert.equal(renderMarkdown('   \n\n  '), '');
});

// ----------------------------------------------------------------- planTitle

// planTitle is the other exported symbol: it extracts a card title (first
// heading, else first non-empty line) clipped to 60 chars. It is a plain-text
// extractor — the caller inserts it as text, not HTML — so these pin the
// extraction/clipping contract only.
test('planTitle prefers the first heading', () => {
  assert.equal(planTitle('intro line\n# The Heading\nmore'), 'The Heading');
  assert.equal(planTitle('## second-level\nbody'), 'second-level');
});

test('planTitle falls back to the first non-empty line, then to a placeholder', () => {
  assert.equal(planTitle('\n\n  just a line  \nnext'), 'just a line');
  assert.equal(planTitle(''), 'untitled plan');
  assert.equal(planTitle('   \n  '), 'untitled plan');
});

test('planTitle clips to 60 characters with an ellipsis', () => {
  const long = '# ' + 'x'.repeat(100);
  const title = planTitle(long);
  assert.equal(title.length, 58, '57 chars + the ellipsis');
  assert.ok(title.endsWith('…'));
});
