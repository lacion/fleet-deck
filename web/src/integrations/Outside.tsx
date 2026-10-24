// R3.1 and R3.3 (DL-164): third-party text on the board. The station wraps it in an envelope for
// models; the board shows it with a muted note, through the chat Markdown (no raw HTML, no image
// loads, links open in a new tab).
import { Markdown } from '../chat/Markdown';
import type { Asset } from '../gen';
import type { IntegrationActions } from './actions';
import { dataUrl, imageAlt, outsideNote, unfence } from './model';
import { type Load, useLoad } from './useLoad';

/** A Markdown text from a system, with the note "From <system>. Text written outside Fleet Deck." */
export function OutsideText({
  system,
  text,
  note = true,
}: {
  readonly system: string;
  readonly text: string;
  /** False when the surrounding section already shows the note. */
  readonly note?: boolean;
}) {
  const body = unfence(text).trim();
  return (
    <div className="fd-int-outside">
      {note ? <p className="fd-note fd-int-outside-note">{outsideNote(system)}</p> : null}
      {body === '' ? <p className="fd-empty">No text.</p> : <Markdown text={body} />}
    </div>
  );
}

/** One image of an issue body: the station fetches it, the board shows it as a `data:` URL. */
export function AssetImage({
  actions,
  project,
  source,
  url,
  alt,
  initial,
}: {
  readonly actions: IntegrationActions;
  readonly project: string;
  readonly source: string | undefined;
  readonly url: string;
  readonly alt: string;
  /** The answer at the first render (tests). */
  readonly initial?: Asset;
}) {
  const { state } = useLoad<Asset>(
    () => actions.asset(project, source, url),
    `${project}|${source ?? ''}|${url}`,
    initial,
  );
  return <AssetView state={state} alt={alt} />;
}

/** The image of a loaded asset; a loading, failed or unsafe image shows the alt text. */
export function AssetView({ state, alt }: { readonly state: Load<Asset>; readonly alt: string }) {
  const src = state.kind === 'ok' ? dataUrl(state.value) : null;
  if (src === null) {
    return (
      <span className="fd-int-image fd-int-image-alt" data-state={state.kind}>
        {alt}
      </span>
    );
  }
  return <img className="fd-int-image" src={src} alt={alt} loading="lazy" decoding="async" />;
}

/** The images of an issue, each loaded through the assets route. */
export function Images({
  actions,
  project,
  source,
  urls,
  body,
}: {
  readonly actions: IntegrationActions;
  readonly project: string;
  readonly source: string | undefined;
  readonly urls: readonly string[];
  /** The Markdown body: `![alt](url)` gives the alt text of an image. */
  readonly body: string;
}) {
  if (urls.length === 0) return null;
  const alts = new Map<string, string>();
  for (const m of body.matchAll(/!\[([^\]]*)\]\(\s*<?([^)\s>]+)/g)) {
    if (m[1] !== undefined && m[1] !== '' && m[2] !== undefined) alts.set(m[2], m[1]);
  }
  return (
    <div className="fd-int-images" aria-label="Images">
      {urls.map((url, i) => (
        <AssetImage
          key={url}
          actions={actions}
          project={project}
          source={source}
          url={url}
          alt={alts.get(url) ?? imageAlt(url, i + 1)}
        />
      ))}
    </div>
  );
}
