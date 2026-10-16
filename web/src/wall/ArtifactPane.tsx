// R13 P1: the artifact mode: one artifact full screen in the existing sandboxed frame. The deck
// gives a one-hour link (`POST /api/artifacts/{station}/{id}/view`); the pane asks again every
// 50 minutes, so the link never ends on a screen that runs all day.
import { useEffect, useState } from 'react';
import type { ArtifactActions } from '../artifacts/actions';
import { ArtifactFrame } from '../artifacts/ArtifactViewer';

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
