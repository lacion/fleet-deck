// DL-147: paste an image into a terminal tab. The board stores the image on the station
// (`POST /api/stations/{station}/paste`) and types the stored path into the terminal.

/** The most bytes that the deck takes for one pasted image (decoded). */
export const MAX_PASTE_BYTES = 716800;

export type ImageExt = 'png' | 'jpg' | 'gif' | 'webp';

export type PreparedImage = { readonly ext: ImageExt; readonly bytes: Uint8Array };

const TYPES: Readonly<Record<string, ImageExt>> = {
  'image/png': 'png',
  'image/jpeg': 'jpg',
  'image/gif': 'gif',
  'image/webp': 'webp',
};

const REENCODE_QUALITY = 0.85;
const MAX_HALVINGS = 4;

export function extOf(mime: string): ImageExt | null {
  return TYPES[mime] ?? null;
}

type Surface = { blob(type: string, quality: number): Promise<Blob | null> };

/** The bitmap drawn at `width` x `height` on an OffscreenCanvas, or on a `<canvas>` without it. */
function draw(bitmap: ImageBitmap, width: number, height: number): Surface | null {
  if (typeof OffscreenCanvas !== 'undefined') {
    const canvas = new OffscreenCanvas(width, height);
    const context = canvas.getContext('2d');
    if (context === null) return null;
    context.drawImage(bitmap, 0, 0, width, height);
    return { blob: (type, quality) => canvas.convertToBlob({ type, quality }) };
  }
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext('2d');
  if (context === null) return null;
  context.drawImage(bitmap, 0, 0, width, height);
  return {
    blob: (type, quality) => new Promise((resolve) => canvas.toBlob(resolve, type, quality)),
  };
}

async function bytesOf(blob: Blob): Promise<Uint8Array> {
  return new Uint8Array(await blob.arrayBuffer());
}

const UNREADABLE = { error: 'The board cannot read this image.' } as const;

/**
 * The image as the deck takes it: a supported type that fits goes through as it is;
 * else it is re-encoded as WebP (JPEG when the browser has no WebP encoder) at 85 %
 * quality, and its width and height are halved up to 4 times until it fits.
 */
export async function prepareImage(file: Blob): Promise<PreparedImage | { error: string }> {
  const ext = extOf(file.type);
  if (ext !== null && file.size <= MAX_PASTE_BYTES) return { ext, bytes: await bytesOf(file) };
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    return UNREADABLE;
  }
  try {
    for (let halvings = 0; halvings <= MAX_HALVINGS; halvings += 1) {
      const scale = 2 ** halvings;
      const surface = draw(
        bitmap,
        Math.max(1, Math.floor(bitmap.width / scale)),
        Math.max(1, Math.floor(bitmap.height / scale)),
      );
      if (surface === null) return UNREADABLE;
      let blob = await surface.blob('image/webp', REENCODE_QUALITY);
      if (blob === null || blob.type !== 'image/webp') {
        blob = await surface.blob('image/jpeg', REENCODE_QUALITY);
      }
      if (blob === null) return UNREADABLE;
      const out = extOf(blob.type);
      if (out === null) return UNREADABLE;
      if (blob.size <= MAX_PASTE_BYTES) return { ext: out, bytes: await bytesOf(blob) };
    }
    return { error: 'The image is too large to paste.' };
  } catch {
    return UNREADABLE;
  } finally {
    bitmap.close();
  }
}

/**
 * The text that types `path` into a terminal: a plain path as it is, any other path in single
 * quotes, then one space. `null` when the path has a control character (it could run a command).
 */
export function insertText(path: string): string | null {
  if (/[\u0000-\u001f\u007f]/.test(path)) return null;
  if (/^[A-Za-z0-9/._+-]+$/.test(path)) return `${path} `;
  return `'${path.replaceAll("'", "'\\''")}' `;
}

/** An upload id: 32 hex characters, so the deck returns the same path for a retry. */
export function newUploadId(): string {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID().replaceAll('-', '');
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

/** Standard base64 of the bytes, built in chunks so that a large image does not overflow the stack. */
export function toBase64(bytes: Uint8Array): string {
  const chunk = 0x8000;
  let binary = '';
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}
