// M3 6-10: pasted or picked images for a prompt (DL-304).
//
// The limits mirror `fleetdeck_proto::input`: the deck refuses an input
// above them with `invalid_input`, so the board checks first and says why.
import type { ImageInput } from '../gen';

export const MAX_IMAGES = 4;
/** Base64 characters of all images of one input together. */
export const MAX_IMAGE_BYTES = 768 * 1024;
export const IMAGE_TYPES: readonly string[] = [
  'image/png',
  'image/jpeg',
  'image/gif',
  'image/webp',
];
/** A larger image is drawn again as JPEG at this many pixels on its long side. */
const SHRINK_EDGE = 1600;

/** `data:<type>;base64,<data>` → an image input, or `null` for another form. */
export function fromDataUrl(url: string): ImageInput | null {
  const match = /^data:([a-z/+.-]+);base64,([A-Za-z0-9+/]+=*)$/.exec(url);
  if (!match?.[1] || !match[2]) return null;
  return { media_type: match[1], data: match[2] };
}

export const toDataUrl = (image: ImageInput): string =>
  `data:${image.media_type};base64,${image.data}`;

/** Why the images cannot go with a prompt, or `null` when they can. */
export function imagesProblem(images: readonly ImageInput[]): string | null {
  if (images.length > MAX_IMAGES) return `A message takes at most ${MAX_IMAGES} images.`;
  const bad = images.find((i) => !IMAGE_TYPES.includes(i.media_type));
  if (bad) return `The image type ${bad.media_type} is not supported.`;
  const total = images.reduce((sum, i) => sum + i.data.length, 0);
  if (total > MAX_IMAGE_BYTES) {
    return `The images are too large together (${kib(total)} KiB; the limit is ${kib(MAX_IMAGE_BYTES)} KiB).`;
  }
  return null;
}

const kib = (n: number): number => Math.ceil((n * 3) / 4 / 1024);

/** The image files of a paste or a file pick. */
export function imageFiles(files: Iterable<File> | ArrayLike<File>): File[] {
  return Array.from(files).filter((f) => f.type.startsWith('image/'));
}

/** Reads a file. A large or unsupported image is drawn again as JPEG when the browser can. */
export async function readImage(file: Blob): Promise<ImageInput | null> {
  const image = { media_type: file.type, data: base64(new Uint8Array(await file.arrayBuffer())) };
  if (IMAGE_TYPES.includes(image.media_type) && image.data.length <= MAX_IMAGE_BYTES / 2) {
    return image;
  }
  return (await shrink(file)) ?? image;
}

function base64(bytes: Uint8Array): string {
  let text = '';
  for (let i = 0; i < bytes.length; i += 0x8000) {
    text += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(text);
}

async function shrink(file: Blob): Promise<ImageInput | null> {
  if (typeof createImageBitmap !== 'function' || typeof document === 'undefined') return null;
  try {
    const bitmap = await createImageBitmap(file);
    const scale = Math.min(1, SHRINK_EDGE / Math.max(bitmap.width, bitmap.height));
    const canvas = document.createElement('canvas');
    canvas.width = Math.round(bitmap.width * scale);
    canvas.height = Math.round(bitmap.height * scale);
    canvas.getContext('2d')?.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    return fromDataUrl(canvas.toDataURL('image/jpeg', 0.8));
  } catch {
    return null;
  }
}
