/** The longest text (in characters) that an OSC 52 may put on the clipboard. */
export const OSC52_MAX = 100_000;

const BASE64 = /^[A-Za-z0-9+/]*={0,2}$/;

/**
 * The text of an OSC 52 write: `data` is `<targets>;<base64>`. A read request
 * (`?`), invalid base64, invalid UTF-8, an empty text and a text over
 * `OSC52_MAX` give null: the caller ignores them.
 */
export function oscCopyText(data: string): string | null {
  const split = data.indexOf(';');
  if (split < 0) return null;
  const payload = data.slice(split + 1);
  if (payload === '' || payload === '?') return null;
  if (payload.length % 4 !== 0 || !BASE64.test(payload)) return null;
  let text: string;
  try {
    const binary = atob(payload);
    const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
    text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
  if (text === '' || text.length > OSC52_MAX) return null;
  return text;
}
