// R12 (DL-165): what the board needs to know about an artifact's files.
import { formatHash } from '../app/routes';
import type { ArtifactKind } from '../gen';

export const KINDS: readonly ArtifactKind[] = ['html', 'markdown', 'image', 'svg', 'pdf', 'other'];

export const KIND_LABEL: Record<ArtifactKind, string> = {
  html: 'HTML',
  markdown: 'Markdown',
  image: 'Image',
  svg: 'SVG',
  pdf: 'PDF',
  other: 'File',
};

/** The kind of a file by its extension: the same table as Rust `ArtifactKind::of_path`. */
export function kindOfPath(path: string): ArtifactKind {
  const name = path.split('/').pop() ?? '';
  const dot = name.lastIndexOf('.');
  const ext = dot < 0 ? '' : name.slice(dot + 1).toLowerCase();
  switch (ext) {
    case 'html':
    case 'htm':
      return 'html';
    case 'md':
    case 'markdown':
      return 'markdown';
    case 'png':
    case 'jpg':
    case 'jpeg':
    case 'gif':
    case 'webp':
    case 'avif':
    case 'bmp':
    case 'ico':
      return 'image';
    case 'svg':
      return 'svg';
    case 'pdf':
      return 'pdf';
    default:
      return 'other';
  }
}

/** A file that "Publish as artifact" offers: HTML, Markdown, images, SVG and PDF. */
export const publishable = (path: string): boolean => kindOfPath(path) !== 'other';

/** The last segment of a path. */
export const fileName = (path: string): string => path.split('/').filter(Boolean).pop() ?? path;

/** The hash link of the full viewer page. */
export const artifactHref = (station: string, id: string, version: number | null = null): string =>
  formatHash({ name: 'artifact', station, id, version });
