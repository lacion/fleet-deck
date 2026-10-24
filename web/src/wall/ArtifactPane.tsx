// R13 P1: the artifact mode: one artifact full screen in the existing sandboxed frame. The deck
// gives a one-hour link (`POST /api/artifacts/{station}/{id}/view`); the pane asks again every
// 50 minutes, so the link never ends on a screen that runs all day.
import { useEffect, useState } from 'react';
import type { ArtifactActions } from '../artifacts/actions';
import { ArtifactFrame } from '../artifacts/ArtifactViewer';

/** The line of the mirror's artifact card (B17 §3.3): the Remote page never loads the file. */
export const MIRROR_ARTIFACT_TEXT = 'The wall shows this artifact. The remote does not open it.';

/**
 * The artifact card of the mirror on the Remote page (B17): the id and the version, and no frame.
 * The mirror asks for no view link, so an agent-made HTML file never runs on the owner's device.
 */
export function ArtifactCard({
  id,
  version,
}: {
  readonly id: string;
  readonly version?: number | undefined;
}) {
  return (
    <section className="fd-wall-pagecard" aria-label="Artifact">
      <h2 className="fd-wall-h">Artifact</h2>
      <p className="fd-wall-pagehost">
        {version === undefined ? id : `${id} · version ${version}`}
      </p>
      <p className="fd-wall-note">{MIRROR_ARTIFACT_TEXT}</p>
    </section>
  );
}

/** The wait before the link is read again: 50 minutes. */
export const ARTIFACT_REFRESH_MS = 50 * 60_000;

export function ArtifactPane({
  station,
  id,
  artifacts,
  refreshMs = ARTIFACT_REFRESH_MS,
}: {
  readonly station: string;
  readonly id: string;
  readonly artifacts: ArtifactActions | undefined;
  readonly refreshMs?: number;
}) {
  const [url, setUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (artifacts === undefined) return undefined;
    let live = true;
    setUrl(null);
    const load = () => {
      artifacts.view(station, id, null).then(
        (link) => {
          if (!live) return;
          setUrl(link.url);
          setError(null);
        },
        (e: unknown) =>
          live && setError(e instanceof Error ? e.message : 'The artifact did not load.'),
      );
    };
    load();
    const timer = setInterval(load, refreshMs);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [artifacts, station, id, refreshMs]);
  return (
    <section className="fd-wall-artifact" aria-label="Artifact">
      {error !== null ? <p className="fd-wall-note">{error}</p> : null}
      {url !== null ? (
        <ArtifactFrame url={url} title="Artifact" />
      ) : error === null ? (
        <p className="fd-wall-note">Loading…</p>
      ) : null}
    </section>
  );
}
