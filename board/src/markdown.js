// Tiny local markdown renderer for plan cards (v1.3 — simple markdown
// rendering, no new deps). Supported, and nothing more:
//   ``` fenced code blocks · #/##/### headings · - bullet lists ·
//   paragraphs · inline **bold** and `code`.
// EVERYTHING is HTML-escaped before a single tag is added, so plan text
// (agent-authored, untrusted) can never inject markup. Unrecognized syntax
// renders literally — honest, never lossy.

const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

// in-file only — renderMarkdown()/planTitle() are the exported surface.
function escapeHtml(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ESC[c]);
}

// Inline spans over ALREADY-ESCAPED text. `code` wins over **bold** (split on
// code spans first so a ** inside backticks never bolds).
function inline(escaped) {
  return escaped
    .split(/(`[^`]+`)/)
    .map((part) => {
      if (part.length > 2 && part.startsWith('`') && part.endsWith('`')) {
        return '<code>' + part.slice(1, -1) + '</code>';
      }
      // [\s\S] — bold may span a soft line break inside one paragraph
      return part.replace(/\*\*([\s\S]+?)\*\*/g, '<strong>$1</strong>');
    })
    .join('');
}

export function renderMarkdown(md) {
  const lines = String(md ?? '').replace(/\r\n/g, '\n').split('\n');
  const out = [];
  let i = 0;
  const isBlank = (l) => /^\s*$/.test(l);
  const isFence = (l) => /^```/.test(l);
  const isHead = (l) => /^#{1,3}\s+/.test(l);
  const isItem = (l) => /^\s*-\s+/.test(l);

  while (i < lines.length) {
    const line = lines[i];
    if (isBlank(line)) { i += 1; continue; }
    if (isFence(line)) {
      const buf = [];
      i += 1;
      while (i < lines.length && !isFence(lines[i])) { buf.push(lines[i]); i += 1; }
      i += 1; // closing fence (or EOF — an unclosed fence still renders)
      out.push('<pre><code>' + escapeHtml(buf.join('\n')) + '</code></pre>');
      continue;
    }
    const h = /^(#{1,3})\s+(.*)$/.exec(line);
    if (h) {
      const lvl = h[1].length;
      out.push(`<h${lvl}>` + inline(escapeHtml(h[2].trim())) + `</h${lvl}>`);
      i += 1;
      continue;
    }
    if (isItem(line)) {
      const items = [];
      while (i < lines.length && isItem(lines[i])) {
        items.push('<li>' + inline(escapeHtml(lines[i].replace(/^\s*-\s+/, ''))) + '</li>');
        i += 1;
      }
      out.push('<ul>' + items.join('') + '</ul>');
      continue;
    }
    // paragraph: consecutive lines that are none of the above (newlines kept —
    // .fd-md p is pre-wrap, plan prose often relies on line breaks)
    const buf = [];
    while (i < lines.length && !isBlank(lines[i]) && !isFence(lines[i]) && !isHead(lines[i]) && !isItem(lines[i])) {
      buf.push(lines[i]);
      i += 1;
    }
    out.push('<p>' + inline(escapeHtml(buf.join('\n'))) + '</p>');
  }
  return out.join('');
}

// Library card title: first #/##/### heading, else first non-empty line;
// clipped to 60 chars either way (contract v1.3).
export function planTitle(md) {
  const lines = String(md ?? '').split('\n');
  let title = null;
  for (const l of lines) {
    const h = /^#{1,3}\s+(.*)$/.exec(l.trim());
    if (h) { title = h[1].trim(); break; }
  }
  if (!title) title = (lines.find((l) => l.trim()) || '').trim() || 'untitled plan';
  return title.length > 60 ? title.slice(0, 57) + '…' : title;
}
