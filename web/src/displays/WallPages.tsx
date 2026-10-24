// B17 §3.2.8 (design `docs/v2/design/wall-control.md`): Settings › Displays › "Wall pages". The
// owner's allow-list of pages that a wall may show in a closed frame: one form to add a page (it asks
// for the elevation the same way as "Add a wall display"), and the list with "Delete…". The deck
// checks every entry; its texts show under the field.
import { type FormEvent, type Ref, useCallback, useEffect, useRef, useState } from 'react';
import type { NewWallPage, PageFit, WallPage } from '../gen';
import type { SettingsActions } from '../settings/actions';
import type { ElevationAsk } from '../settings/elevation';
import { DeckError, describe } from '../settings/http';
import { NoticeIcon } from '../settings/NoticeIcon';
import { Icon, RefreshCw } from '../ui/icons';
import { Field } from '../ui/page';
import { hostOf } from '../wall/PagePane';
import type { DisplayActions } from './actions';
import { withElevation } from './elevate';

/** The feature token of the deck that has the wall pages (`GET /api/features`). */
export const WALL_PAGES_FEATURE = 'wall_pages.v1';

export const HEADING_TEXT = 'Wall pages';
export const INTRO_TEXT =
  'Pages that a wall may show besides Fleet Deck views. A wall shows a page in a closed frame: the page gets no login and no cookies of the wall, and it cannot open windows or leave the frame. A page can show parts of other sites inside itself. Add only sites that you trust.';
export const FRAMING_NOTE =
  'Some sites refuse to show inside another page. Open those with Desktop › Open a link.';
export const HTTP_NOTE =
  "Use https addresses. Plain http works only for pages on the wall's own machine (localhost).";
export const SECRET_WARNING =
  "Do not put a password or a token in an address. Programs that run as you on the wall's machine can read it.";
export const EMPTY_TEXT = 'No wall pages yet.';
export const DELETE_CONFIRM = 'Delete this page? Walls that show it go back to Mission view.';
/** The deck text of a name that is not 1 to 60 characters (§3.2.3): it shows under "Name". */
export const NAME_PROBLEM = 'Give a name of 1 to 60 characters.';

/** The words of a page fit in the list. */
export const FIT_TEXT: Readonly<Record<PageFit, string>> = {
  screen: 'One screen',
  long: 'Long page',
};

export type PagesState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'ready'; readonly pages: readonly WallPage[] };

/** The allow-list. It loads at once and again after `refresh`. */
export function useWallPages(actions: DisplayActions): {
  readonly state: PagesState;
  readonly refresh: () => void;
} {
  const [state, setState] = useState<PagesState>({ kind: 'loading' });
  const on = useRef(true);
  const refresh = useCallback(() => {
    actions.wallPages().then(
      (list) => on.current && setState({ kind: 'ready', pages: list.pages }),
      (e: unknown) => on.current && setState({ kind: 'error', text: describe(e) }),
    );
  }, [actions]);
  useEffect(() => {
    on.current = true;
    refresh();
    return () => {
      on.current = false;
    };
  }, [refresh]);
  return { state, refresh };
}

export type PageForm = {
  readonly label: string;
  readonly url: string;
  readonly fit: PageFit;
};

/** The answer of an add: added, cancelled in the elevation dialog, or a text for a field. */
export type AddResult =
  | { readonly kind: 'added' }
  | { readonly kind: 'cancelled' }
  | { readonly kind: 'error'; readonly field: 'name' | 'address' | null; readonly text: string };

/**
 * Adds a page. The deck answers `400 invalid_page` with the text of the first failed check: the name
 * text shows under "Name", every other `invalid_page` text under "Address". Another error has no
 * field.
 */
export async function addPage(
  actions: DisplayActions,
  settings: SettingsActions,
  ask: ElevationAsk,
  form: PageForm,
): Promise<AddResult> {
  const page: NewWallPage = { label: form.label.trim(), url: form.url.trim(), fit: form.fit };
  try {
    const added = await withElevation(settings, ask, (elevation) =>
      actions.addWallPage(page, elevation),
    );
    return added === null ? { kind: 'cancelled' } : { kind: 'added' };
  } catch (e) {
    const text = describe(e);
    if (e instanceof DeckError && e.code === 'invalid_page') {
      return { kind: 'error', field: text === NAME_PROBLEM ? 'name' : 'address', text };
    }
    return { kind: 'error', field: null, text };
  }
}

/** The form that adds a page. */
export function AddPageForm({
  actions,
  settings,
  ask,
  onAdded,
}: {
  readonly actions: DisplayActions;
  readonly settings: SettingsActions;
  readonly ask: ElevationAsk;
  readonly onAdded: () => void;
}) {
  const [label, setLabel] = useState('');
  const [url, setUrl] = useState('');
  const [fit, setFit] = useState<PageFit>('screen');
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<{
    readonly field: 'name' | 'address' | null;
    readonly text: string;
  } | null>(null);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setProblem(null);
    addPage(actions, settings, ask, { label, url, fit }).then((out) => {
      setBusy(false);
      if (out.kind === 'error') setProblem({ field: out.field, text: out.text });
      if (out.kind !== 'added') return;
      setLabel('');
      setUrl('');
      setFit('screen');
      onAdded();
    });
  };
  return (
    <form className="fd-set-dialogform fd-wp-form" onSubmit={submit} aria-label="Add a wall page">
      <Field label="Name" layout="row" error={problem?.field === 'name' ? problem.text : null}>
        <input value={label} onChange={(e) => setLabel(e.target.value)} />
      </Field>
      <Field
        label="Address"
        layout="row"
        error={problem?.field === 'address' ? problem.text : null}
      >
        <input
          type="url"
          value={url}
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => setUrl(e.target.value)}
        />
      </Field>
      <Field label="Fit" layout="row" group>
        <span className="fd-disp-modes">
          <label className="fd-disp-check">
            <input
              type="radio"
              name="wall-page-fit"
              checked={fit === 'screen'}
              onChange={() => setFit('screen')}
            />{' '}
            Fits one screen
          </label>
          <label className="fd-disp-check">
            <input
              type="radio"
              name="wall-page-fit"
              checked={fit === 'long'}
              onChange={() => setFit('long')}
            />{' '}
            Long page (scrolls)
          </label>
        </span>
      </Field>
      {problem?.field === null ? (
        <p className="fd-fielderror" role="alert">
          {problem.text}
        </p>
      ) : null}
      <div className="fd-formactions">
        <button type="submit" className="fd-primary" disabled={busy}>
          Add the page
        </button>
      </div>
    </form>
  );
}

/** The confirmation of a delete: its text, and the two answers. "Keep" is the safe one. */
export function DeleteConfirm({
  busy,
  keepRef,
  onDelete,
  onKeep,
}: {
  readonly busy: boolean;
  readonly keepRef?: Ref<HTMLButtonElement>;
  readonly onDelete: () => void;
  readonly onKeep: () => void;
}) {
  return (
    <div className="fd-disp-confirm fd-wp-confirm" role="alertdialog" aria-label="Delete this page">
      <p>{DELETE_CONFIRM}</p>
      <button type="button" className="fd-danger" disabled={busy} onClick={onDelete}>
        Delete
      </button>
      <button type="button" className="fd-ghost" ref={keepRef} disabled={busy} onClick={onKeep}>
        Keep
      </button>
    </div>
  );
}

/** One page of the list, with "Delete…" and its confirmation. */
export function PageRow({
  page,
  onDelete,
}: {
  readonly page: WallPage;
  /** Deletes the page; the answer is the text of a failure, or `null`. */
  readonly onDelete: (id: string) => Promise<string | null>;
}) {
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const keep = useRef<HTMLButtonElement>(null);
  // The confirmation opens on its safe answer.
  useEffect(() => {
    if (confirm) keep.current?.focus();
  }, [confirm]);
  const remove = () => {
    setBusy(true);
    setError(null);
    onDelete(page.page_id).then((text) => {
      setBusy(false);
      setConfirm(false);
      setError(text);
    });
  };
  return (
    <li className="fd-wp-row" data-page={page.page_id}>
      <span className="fd-wp-name">{page.label}</span>
      <span className="fd-note fd-wp-host">{hostOf(page.origin)}</span>
      <span className="fd-note fd-wp-fit">{FIT_TEXT[page.fit]}</span>
      <button
        type="button"
        className="fd-danger fd-sm fd-wp-delete"
        disabled={busy}
        onClick={() => setConfirm(true)}
      >
        Delete…
      </button>
      {confirm ? (
        <DeleteConfirm
          busy={busy}
          keepRef={keep}
          onDelete={remove}
          onKeep={() => setConfirm(false)}
        />
      ) : null}
      {error === null ? null : (
        <p className="fd-fielderror fd-wp-error" role="alert">
          {error}
        </p>
      )}
    </li>
  );
}

/** The list for a state: the entries, the empty text, or the load error. */
export function PageList({
  state,
  onDelete,
  onRetry,
}: {
  readonly state: PagesState;
  readonly onDelete: (id: string) => Promise<string | null>;
  readonly onRetry: () => void;
}) {
  if (state.kind === 'loading') {
    return (
      <p className="fd-note fd-set-loading" role="status">
        Loading wall pages…
      </p>
    );
  }
  if (state.kind === 'error') {
    return (
      <div className="fd-set-notice" data-tone="error" role="alert">
        <NoticeIcon tone="error" />
        <span>{state.text}</span>
        <button type="button" className="fd-sm" onClick={onRetry}>
          <Icon of={RefreshCw} size={14} /> Try again
        </button>
      </div>
    );
  }
  if (state.pages.length === 0) return <p className="fd-empty">{EMPTY_TEXT}</p>;
  return (
    <ul className="fd-wp-list">
      {state.pages.map((p) => (
        <PageRow key={p.page_id} page={p} onDelete={onDelete} />
      ))}
    </ul>
  );
}

/** The section "Wall pages". It is the owner's page: the caller shows it only with the feature. */
export function WallPages({
  actions,
  settings,
  ask,
}: {
  readonly actions: DisplayActions;
  readonly settings: SettingsActions;
  readonly ask: ElevationAsk;
}) {
  const { state, refresh } = useWallPages(actions);
  const onDelete = async (id: string): Promise<string | null> => {
    try {
      await actions.deleteWallPage(id);
      refresh();
      return null;
    } catch (e) {
      return describe(e);
    }
  };
  return (
    <section aria-label={HEADING_TEXT} className="fd-panel fd-wp">
      <h2 className="fd-form-title fd-disp-title">{HEADING_TEXT}</h2>
      <p className="fd-note fd-wp-intro">{INTRO_TEXT}</p>
      <p className="fd-note">{FRAMING_NOTE}</p>
      <p className="fd-note">{HTTP_NOTE}</p>
      <p className="fd-note fd-wp-hazard">{SECRET_WARNING}</p>
      <AddPageForm actions={actions} settings={settings} ask={ask} onAdded={refresh} />
      <PageList state={state} onDelete={onDelete} onRetry={refresh} />
    </section>
  );
}
