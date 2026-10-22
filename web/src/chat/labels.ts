/** Who started a run; `continuation` is the station's restart input (ADR-007 §2.5). */
export function runLabel(origin: string): string {
  if (origin === 'provider') return 'Provider turn';
  if (origin === 'continuation') return 'Station continuation';
  if (origin === 'shell') return 'Shell command';
  return 'You';
}

/**
 * The name of a tool row. An MCP tool (`mcp__<server>__<tool>`) reads as its tool in sentence
 * case, with its server apart (`mcp__fleetdeck__publish_artifact` → "Publish artifact",
 * "fleetdeck"); any other title stays as it is.
 */
export function toolName(title: string): { name: string; server: string | null } {
  const mcp = /^mcp__(.+?)__(.+)$/.exec(title);
  const server = mcp?.[1];
  const words = mcp?.[2]?.replace(/[_-]+/g, ' ').trim();
  if (server === undefined || words === undefined || words === '') {
    return { name: title, server: null };
  }
  return { name: words.charAt(0).toUpperCase() + words.slice(1), server };
}

const firstLine = (text: string): string => text.split('\n')[0] ?? '';

/** The text of a JSON string body; a body that a cut left with a broken escape at its end loses it. */
function unescapeJson(body: string): string {
  for (const text of [body, body.replace(/\\(?:u[0-9a-fA-F]{0,3})?(…?)$/, '$1')]) {
    try {
      return JSON.parse(`"${text}"`) as string;
    } catch {
      // try the next form
    }
  }
  return body;
}

/**
 * The plain values (strings, numbers, booleans) at the top level of a JSON object text, in their
 * order. The station cuts a long input at 200 characters (`…`): such a text gives the values
 * before the cut, the last one with its `…`. Nested objects and arrays are left out.
 */
function topValues(text: string): string[] {
  const values: string[] = [];
  let depth = 0;
  let value = false; // after a `:` at the top level
  for (let i = 0; i < text.length; i++) {
    const c = text[i] ?? '';
    if (c === '"') {
      let end = i + 1;
      while (end < text.length && text[end] !== '"') end += text[end] === '\\' ? 2 : 1;
      if (depth === 1 && value) values.push(unescapeJson(text.slice(i + 1, end)));
      value = false;
      i = end;
    } else if (c === '{' || c === '[') {
      depth++;
      value = false;
    } else if (c === '}' || c === ']') {
      depth--;
    } else if (c === ':') {
      value = depth === 1;
    } else if (c === ',') {
      value = false;
    } else if (depth === 1 && value && /[-\w.]/.test(c)) {
      const token = /^[-\w.+]+/.exec(text.slice(i))?.[0] ?? c;
      if (token !== 'null') values.push(token);
      value = false;
      i += token.length - 1;
    }
  }
  return values;
}

/**
 * The input of a tool call as one row. A JSON object (whole, or cut by the station) reads as its
 * plain values (`{"task_id":"bv93ogg6d"}` → "bv93ogg6d"); any other input is its first line. The
 * expanded row keeps the raw input.
 */
export function argText(input: string): string {
  if (!input.trimStart().startsWith('{')) return firstLine(input);
  const values = topValues(input)
    .map((v) => v.replace(/\s+/g, ' ').trim())
    .filter((v) => v !== '');
  return values.length > 0 ? values.join(' · ') : firstLine(input);
}

/** A duration in the words of the board (DESIGN-SYSTEM §5): "12 s", "2 min", "3 h". */
export function durationText(ms: number): string {
  const seconds = Math.max(1, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds} s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  return `${Math.floor(minutes / 60)} h`;
}

/**
 * The summary of a Thinking row (DESIGN-SYSTEM §4.5): "Thinking…" while it streams, "Thinking ·
 * 12 s" when it is done. The time runs from the first to the last delta (Unix ms); without a
 * span (one delta, or no times) the row says only "Thinking".
 */
export function thinkingText(item: {
  readonly done: boolean;
  readonly started_at?: number;
  readonly ended_at?: number;
}): string {
  if (!item.done) return 'Thinking…';
  const { started_at: start, ended_at: end } = item;
  if (start === undefined || end === undefined || end <= start) return 'Thinking';
  return `Thinking · ${durationText(end - start)}`;
}
