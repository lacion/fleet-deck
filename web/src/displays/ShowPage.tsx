// B17 §3.2.8 (design `docs/v2/design/wall-control.md`): "Show a page…" in the menu of a display
// row. The dialog "Show a page on <display name>" lists the wall pages; "Show the page" sends the
// directive `{ mode: "page", page: { page_id }, pin, filter: null }` (an owner directive).
import { type FormEvent, useEffect, useState } from 'react';
import type { DirectiveBody, DirectiveView, DisplayView, WallPage } from '../gen';
import { Dialog } from '../integrations/Dialog';
import { describe } from '../settings/http';
import type { DisplayActions } from './actions';

export const KEEP_TEXT = 'Keep it on the screen (no auto-focus)';
export const SHOWN_TEXT = 'The wall shows the page.';
export const NO_PAGES_TEXT = 'No wall pages yet. Add one under Wall pages.';

/** The directive for a page: `filter` is none (the ceiling of the display stays). */
export const pageDirective = (pageId: string, pin: boolean): DirectiveBody => ({
  mode: 'page',
  page: { page_id: pageId },
  pin,
  filter: null,
});

/** Sends the page directive to a display (an owner directive). */
export const sendPage = (
  actions: Pick<DisplayActions, 'directive'>,
  displayId: string,
  pageId: string,
  pin: boolean,
): Promise<DirectiveView> => actions.directive(displayId, pageDirective(pageId, pin));

export type Loaded =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'ready'; readonly pages: readonly WallPage[] };

/** The body of the dialog for a state of the list (the tests render it without the deck). */
export function ShowPageForm({
  loaded,
  chosen,
  pin,
  busy,
  error,
  shown,
  onChoose,
  onPin,
  onSubmit,
  onCancel,
}: {
  readonly loaded: Loaded;
  readonly chosen: string;
  readonly pin: boolean;
  readonly busy: boolean;
  readonly error: string | null;
  readonly shown: boolean;
  readonly onChoose: (pageId: string) => void;
  readonly onPin: (pin: boolean) => void;
  readonly onSubmit: (event: FormEvent) => void;
  readonly onCancel: () => void;
}) {
  const pages = loaded.kind === 'ready' ? loaded.pages : [];
  return (
    <form className="fd-set-dialogform" onSubmit={onSubmit} aria-label="Show a page">
      {loaded.kind === 'loading' ? <p role="status">Loading…</p> : null}
      {loaded.kind === 'error' ? (
        <p className="fd-fielderror" role="alert">
          {loaded.text}
        </p>
      ) : null}
      {loaded.kind === 'ready' && pages.length === 0 ? (
        <p className="fd-note">{NO_PAGES_TEXT}</p>
      ) : null}
      {pages.length === 0 ? null : (
        <>
          <label className="fd-fieldcontrol">
            <span className="fd-fieldlabel">Page</span>
            <select value={chosen} onChange={(e) => onChoose(e.target.value)}>
              {pages.map((p) => (
                <option key={p.page_id} value={p.page_id}>
                  {p.label}
                </option>
              ))}
            </select>
          </label>
          <label className="fd-disp-check">
            <input type="checkbox" checked={pin} onChange={(e) => onPin(e.target.checked)} />{' '}
            {KEEP_TEXT}
          </label>
        </>
      )}
      {error === null ? null : (
        <p className="fd-fielderror" role="alert">
          {error}
        </p>
      )}
      {shown ? <p role="status">{SHOWN_TEXT}</p> : null}
      <div className="fd-formactions">
        {/* After the directive the head X closes the dialog (§4.7: no text Close button). */}
        {shown ? null : (
          <button type="button" className="fd-ghost" onClick={onCancel}>
            Cancel
          </button>
        )}
        <button
          type="submit"
          className="fd-primary"
          disabled={busy || chosen === '' || pages.length === 0}
        >
          Show the page
        </button>
      </div>
    </form>
  );
}

export function ShowPageDialog({
  display,
  actions,
  onClose,
}: {
  readonly display: Pick<DisplayView, 'display_id' | 'name'>;
  readonly actions: DisplayActions;
  readonly onClose: () => void;
}) {
  const [loaded, setLoaded] = useState<Loaded>({ kind: 'loading' });
  const [chosen, setChosen] = useState('');
  const [pin, setPin] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shown, setShown] = useState(false);
  useEffect(() => {
    let live = true;
    actions.wallPages().then(
      (list) => {
        if (!live) return;
        setLoaded({ kind: 'ready', pages: list.pages });
        setChosen(list.pages[0]?.page_id ?? '');
      },
      (e: unknown) => live && setLoaded({ kind: 'error', text: describe(e) }),
    );
    return () => {
      live = false;
    };
  }, [actions]);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy || chosen === '') return;
    setBusy(true);
    setError(null);
    sendPage(actions, display.display_id, chosen, pin).then(
      () => {
        setBusy(false);
        setShown(true);
      },
      (e: unknown) => {
        setBusy(false);
        setError(describe(e));
      },
    );
  };
  return (
    <Dialog title={`Show a page on ${display.name}`} onClose={onClose}>
      <ShowPageForm
        loaded={loaded}
        chosen={chosen}
        pin={pin}
        busy={busy}
        error={error}
        shown={shown}
        onChoose={setChosen}
        onPin={setPin}
        onSubmit={submit}
        onCancel={onClose}
      />
    </Dialog>
  );
}
