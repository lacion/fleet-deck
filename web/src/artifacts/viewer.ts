// R12 (DL-165): the data of the artifact viewer. The pure pieces (`mainFile`, `selectedVersion`)
// are tested; `useViewer` runs the calls.
import { useEffect, useState } from 'react';
import type { ArtifactVersion, DeckArtifactDetail } from '../gen';
import type { ArtifactActions } from './actions';

/** One thing that loads: it is loading, it failed with a text, or it has a value. */
export type Part<T> =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'ok'; readonly value: T };

export type ViewerReady = {
  readonly detail: DeckArtifactDetail;
  /** The version that the viewer shows. */
  readonly version: number;
  /** The one-hour link of the sandboxed frame (`/a/<token>/`). */
  readonly link: Part<string>;
  /** The text of the main file; `null` unless the artifact is Markdown. */
  readonly text: Part<string> | null;
};

export type ViewerData = Part<ViewerReady>;

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/** The version to show: the wanted one, else the latest. */
export const selectedVersion = (detail: DeckArtifactDetail, wanted: number | null): number =>
  wanted ?? detail.artifact.latest_version;

/** The row of one version in the detail; a version with no row is unknown. */
export const versionRow = (
  detail: DeckArtifactDetail,
  version: number,
): ArtifactVersion | undefined => detail.versions.find((v) => v.version === version);

/** The main file of a version (the first one); `undefined` while the deck has no file list. */
export const mainFile = (detail: DeckArtifactDetail, version: number): string | undefined =>
  versionRow(detail, version)?.files[0];

/**
 * Loads the artifact, the view link of the shown version and, for Markdown, the text.
 * `reload` makes the hook read the detail again (after a share changed).
 */
export function useViewer(
  actions: ArtifactActions,
  station: string,
  id: string,
  wanted: number | null,
  reload: number,
): ViewerData {
  const [detail, setDetail] = useState<Part<DeckArtifactDetail>>({ kind: 'loading' });
  const [link, setLink] = useState<Part<string>>({ kind: 'loading' });
  const [text, setText] = useState<Part<string>>({ kind: 'loading' });

  useEffect(() => {
    let live = true;
    // The caller keys the viewer by artifact: a reload keeps what is on the screen.
    actions.detail(station, id).then(
      (value) => live && setDetail({ kind: 'ok', value }),
      (e: unknown) => live && setDetail({ kind: 'error', text: message(e) }),
    );
    return () => {
      live = false;
    };
  }, [actions, station, id, reload]);

  const ready = detail.kind === 'ok' ? detail.value : null;
  const version = ready === null ? null : selectedVersion(ready, wanted);
  const markdown = ready?.artifact.kind === 'markdown';
  const main = ready === null || version === null ? undefined : mainFile(ready, version);

  useEffect(() => {
    if (version === null) return undefined;
    let live = true;
    setLink({ kind: 'loading' });
    actions.view(station, id, version).then(
      (v) => live && setLink({ kind: 'ok', value: v.url }),
      (e: unknown) => live && setLink({ kind: 'error', text: message(e) }),
    );
    return () => {
      live = false;
    };
  }, [actions, station, id, version]);

  useEffect(() => {
    if (!markdown || version === null) return undefined;
    if (main === undefined) {
      setText({
        kind: 'error',
        text: 'This version has no files yet. The station has not sent them to the deck.',
      });
      return undefined;
    }
    let live = true;
    setText({ kind: 'loading' });
    actions.text(station, id, version, main).then(
      (value) => live && setText({ kind: 'ok', value }),
      (e: unknown) => live && setText({ kind: 'error', text: message(e) }),
    );
    return () => {
      live = false;
    };
  }, [actions, station, id, version, markdown, main]);

  if (detail.kind === 'loading' || detail.kind === 'error') return detail;
  if (version === null) return { kind: 'loading' };
  return {
    kind: 'ok',
    value: { detail: detail.value, version, link, text: markdown ? text : null },
  };
}
