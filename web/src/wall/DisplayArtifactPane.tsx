// R13 P2c: the artifact mode of a wall display (design §5D item 1, §5E item 3, R15 Q2). The deck
// answers one file of one version with an inert policy (a `sandbox` with no scripts). This pane
// probes the answer, reads the kind from its `Content-Type`, and shows:
// - HTML in a frame with an empty `sandbox` (no scripts, an opaque origin);
// - an image or an SVG in an `<img>` (a script never runs there);
// - Markdown as read-only text (no HTML, no link, no image);
// - anything else, and a multi-file artifact, as the card "Open this artifact on your own device".
import { type Ref, useEffect, useRef, useState } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { artifactPath, DISPLAY_HEADER } from '../display/api';
import { longFrameTop, scrollBehavior } from './control';
import type { DisplayEnv } from './displayChanges';

/** The most bytes of a Markdown file that the pane reads. */
export const MARKDOWN_MAX_BYTES = 1024 * 1024;

export const CARD_TEXT = 'Open this artifact on your own device';

export type State =
  | { readonly kind: 'loading' }
  | { readonly kind: 'frame'; readonly url: string }
  | { readonly kind: 'image'; readonly url: string }
  | { readonly kind: 'markdown'; readonly text: string }
  | { readonly kind: 'card' }
  | { readonly kind: 'note'; readonly text: string };

/** The text of a body, at most `max` bytes. */
export async function readCapped(res: Response, max: number): Promise<string> {
  const reader = res.body?.getReader();
  if (reader === undefined) return (await res.text()).slice(0, max);
  const decoder = new TextDecoder();
  let out = '';
  let bytes = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    bytes += value.byteLength;
    out += decoder.decode(value, { stream: true });
    if (bytes >= max) {
      await reader.cancel();
      break;
    }
  }
  return out + decoder.decode();
}

async function errorCode(res: Response): Promise<string | null> {
  try {
    const v: unknown = await res.json();
    return typeof v === 'object' && v !== null && 'error' in v && typeof v.error === 'string'
      ? v.error
      : null;
  } catch {
    return null;
  }
}

/** What an error answer of the route means for the pane. */
export async function failureState(res: Response): Promise<State> {
  const code = await errorCode(res);
  if (code === 'not_displayable') return { kind: 'card' };
  if (res.status === 404) return { kind: 'note', text: 'This artifact is not available.' };
  if (code === 'station_unreachable')
    return { kind: 'note', text: 'The station is not connected.' };
  if (res.status === 401) return { kind: 'note', text: 'The display login ended.' };
  return { kind: 'note', text: 'The artifact did not load.' };
}

/** The state for a 200 answer: the element that fits its content type. */
export async function successState(res: Response, url: string, stop: () => void): Promise<State> {
  const mime = (res.headers.get('content-type') ?? '').split(';')[0]?.trim().toLowerCase() ?? '';
  if (mime === 'text/html') {
    stop();
    return { kind: 'frame', url };
  }
  if (mime.startsWith('image/')) {
    stop();
    return { kind: 'image', url };
  }
  if (mime === 'text/markdown') {
    return { kind: 'markdown', text: await readCapped(res, MARKDOWN_MAX_BYTES) };
  }
  stop();
  return { kind: 'card' };
}

/**
 * Markdown as read-only text: no HTML, no link, no image (a same-origin image URL is a GET with the
 * display cookie), and a task list item as text.
 */
export function DisplayMarkdown({ text }: { readonly text: string }) {
  return (
    <div className="fd-art-md" data-wall-scroll="artifact">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        skipHtml
        components={{
          a: ({ children }) => <span>{children}</span>,
          img: ({ alt }) => <span>{alt ?? ''}</span>,
          input: ({ checked }) => <span>{checked === true ? '[x] ' : '[ ] '}</span>,
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}

/**
 * What the pane shows for a state. An HTML frame sits in a box with the one-screen fit; while
 * `tenths` (`frame_tenths` of the remote, B17) is above 0 the box has the long fit: the frame is 8
 * screens tall and the box shows the part at `tenths`.
 */
export function ArtifactBody({
  state,
  tenths,
  fitRef,
}: {
  readonly state: State;
  readonly tenths: number;
  readonly fitRef?: Ref<HTMLDivElement>;
}) {
  return (
    <>
      {state.kind === 'loading' ? <p className="fd-wall-note">Loading…</p> : null}
      {state.kind === 'note' ? <p className="fd-wall-note">{state.text}</p> : null}
      {state.kind === 'card' ? <p className="fd-wall-card">{CARD_TEXT}</p> : null}
      {state.kind === 'frame' ? (
        <div className="fd-wall-fit" data-fit={tenths > 0 ? 'long' : 'one'} ref={fitRef}>
          <iframe
            className="fd-art-frame"
            title="Artifact"
            src={state.url}
            sandbox=""
            referrerPolicy="no-referrer"
          />
        </div>
      ) : null}
      {state.kind === 'image' ? <img className="fd-art-image" src={state.url} alt="" /> : null}
      {state.kind === 'markdown' ? <DisplayMarkdown text={state.text} /> : null}
    </>
  );
}

export function DisplayArtifactPane({
  station,
  id,
  version,
  env,
  frameTenths = 0,
}: {
  readonly station: string;
  readonly id: string;
  readonly version: number;
  readonly env: DisplayEnv | undefined;
  /** The position of a long-fit frame, in tenths of a screen (`WallRemoteState.frame_tenths`). */
  readonly frameTenths?: number;
}) {
  const [state, setState] = useState<State>({ kind: 'loading' });
  const url = artifactPath(station, id, version);
  const fetcher = env?.fetcher;
  const has = env !== undefined;
  useEffect(() => {
    if (!has) return undefined;
    const controller = new AbortController();
    let live = true;
    setState({ kind: 'loading' });
    const send = fetcher ?? ((path: string, init: RequestInit) => fetch(path, init));
    send(url, {
      method: 'GET',
      credentials: 'same-origin',
      cache: 'no-store',
      redirect: 'error',
      headers: { [DISPLAY_HEADER]: '1' },
      signal: controller.signal,
    }).then(
      async (res) => {
        const next = res.ok
          ? await successState(res, url, () => controller.abort())
          : await failureState(res);
        if (live) setState(next);
      },
      () => live && setState({ kind: 'note', text: 'The artifact did not load.' }),
    );
    return () => {
      live = false;
      controller.abort();
    };
  }, [has, fetcher, url]);
  // A long-fit frame sits at the absolute position of `frameTenths`, never relative to the
  // current position: a second step during a smooth scroll lands on the same place.
  const fit = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = fit.current;
    if (el === null || frameTenths <= 0) return;
    el.scrollTo({ top: longFrameTop(frameTenths, el.clientHeight), behavior: scrollBehavior() });
  }, [frameTenths, state.kind]);
  return (
    <section className="fd-wall-artifact" aria-label="Artifact">
      <ArtifactBody state={state} tenths={frameTenths} fitRef={fit} />
    </section>
  );
}
