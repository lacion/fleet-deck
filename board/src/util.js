// Small pure helpers shared across the board.

export function human(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return s + 's';
  const m = Math.floor(s / 60);
  if (m < 60) return m + 'm';
  return Math.floor(m / 60) + 'h ' + (m % 60) + 'm';
}

export function hhmmss(t) {
  const d = new Date(t);
  const p = (n) => String(n).padStart(2, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

export function basename(p) {
  const s = String(p ?? '');
  const i = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'));
  return i === -1 ? s : s.slice(i + 1);
}

// 'claude-fable-5-20250929' → 'Fable 5' ·  'Fable 5 mini' stays as-is.
export function prettyModel(model) {
  const raw = String(model ?? '').trim();
  if (!raw) return '—';
  const words = raw
    .replace(/^claude[-_ ]/i, '')
    .split(/[-_ ]+/)
    .filter((w) => w && !/^\d{6,}$/.test(w) && !/^v?\d+[a-z]\d*$/i.test(w));
  return words.map((w) => (/^\d/.test(w) ? w : w[0].toUpperCase() + w.slice(1))).join(' ') || raw;
}

export function modelShort(model) {
  return prettyModel(model)
    .split(' ')
    .map((w) => (/^\d/.test(w) ? w : w[0]))
    .join('');
}

// model family → CSS class carrying the --m-* token pair
export function modelFamily(model) {
  const m = String(model ?? '').toLowerCase();
  if (m.includes('fable')) return 'fable';
  if (m.includes('quill')) return 'quill';
  if (m.includes('comet')) return 'comet';
  return 'other';
}

// Daemon columns → board columns. `needsyou` renders in WORKING with amber
// treatment (the question itself lives in the rail — F1/F6: attention is
// global, the card just shows the session is blocked on you).
export const COLS = [
  { key: 'queued', label: 'QUEUED' },
  { key: 'working', label: 'WORKING' },
  { key: 'verifying', label: 'VERIFYING' },
  { key: 'idle', label: 'IDLE' },
  { key: 'offline', label: 'OFFLINE' },
];

export function boardCol(col) {
  if (col === 'needsyou') return 'working';
  return COLS.some((c) => c.key === col) ? col : 'idle';
}

// Ticker rows are {at, msg} only — the tag is derived from the daemon's
// message conventions (derive.mjs / questions.mjs tick() calls).
export function classifyTicker(msg) {
  const m = String(msg ?? '');
  if (m.startsWith('⚠')) return 'confl';
  if (/^(🖐|❓|⌛|✅|💬)/.test(m)) return 'ask';
  if (/^(✉|📣|📌|📝)/.test(m)) return 'mail';
  if (/joined the fleet|left the fleet/.test(m)) return 'join';
  return 'tool';
}

export function stripTickerEmoji(msg) {
  return String(msg ?? '').replace(/^(⚠|🖐|❓|⌛|✅|💬|✉|📣|📌|📝)\s*/u, '');
}

// Question title + command block per kind, from the raw hook payload.
export function questionView(q) {
  const p = q.payload || {};
  if (q.kind === 'permission') {
    const tool = p.tool_name || 'tool';
    const input = p.tool_input || {};
    if (tool === 'Bash' && input.command) {
      const cmd = String(input.command);
      return {
        title: `Run \`${cmd.length > 60 ? cmd.slice(0, 57) + '…' : cmd}\`?`,
        command: '$ ' + cmd,
      };
    }
    if ((tool === 'Edit' || tool === 'MultiEdit') && input.file_path) {
      const olds = String(input.old_string ?? '');
      const news = String(input.new_string ?? '');
      const lines = [
        ...olds.split('\n').filter(Boolean).slice(0, 6).map((l) => ({ kind: 'del', text: '- ' + l })),
        ...news.split('\n').filter(Boolean).slice(0, 6).map((l) => ({ kind: 'add', text: '+ ' + l })),
      ];
      return { title: `Edit ${basename(input.file_path)}?`, command: input.file_path, diff: lines };
    }
    if (tool === 'Write' && input.file_path) {
      const body = String(input.content ?? '').split('\n').slice(0, 6).join('\n');
      return { title: `Write ${basename(input.file_path)}?`, command: `${input.file_path}\n${body}` };
    }
    const pretty = JSON.stringify(input, null, 2);
    return {
      title: `Allow ${tool}?`,
      command: pretty && pretty !== '{}' ? (pretty.length > 400 ? pretty.slice(0, 397) + '…' : pretty) : null,
    };
  }
  if (q.kind === 'choice') {
    const qs = p.tool_input?.questions || [];
    return { title: qs[0]?.question || 'Choose an option', questions: qs };
  }
  if (q.kind === 'freeform') {
    return { title: p.text || '(question lost)' };
  }
  if (q.kind === 'elicitation') {
    return { title: p.message || 'Input requested', schema: p.requestedSchema || null };
  }
  return { title: '(unknown question)' };
}
