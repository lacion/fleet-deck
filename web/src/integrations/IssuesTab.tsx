// R3.3 and R3.4 (DL-164): the Issues tab of a project: search, "Mine", the tracker, the list, the
// preview, and "Start sessions (n)" for the checked rows.
import { useEffect, useState } from 'react';
import { relativeAge } from '../app/sessions';
import type { IssueSummary, Project, StationView } from '../gen';
import { Icon } from '../ui/icons';
import { Loading } from '../ui/Loading';
import { ErrorNotice } from '../ui/page';
import { RefreshCw } from './icons';
import type { IntegrationActions } from './actions';
import { IssuePreview } from './IssuePreview';
import { StartDialog } from './StartDialog';
import { type Load, useLoad } from './useLoad';

const SEARCH_MS = 250;

type Props = {
  readonly project: Project;
  readonly actions: IntegrationActions;
  readonly stations: readonly StationView[];
  readonly now: number;
  /** The list at the first render (tests). */
  readonly initial?: readonly IssueSummary[];
  /** The key of the open preview at the first render (tests). */
  readonly initialOpen?: string | null;
  /** The checked keys at the first render (tests). */
  readonly initialChecked?: readonly string[];
};

/** Why a project has no issue list, or `null`. */
export const noSourceText = (p: Project): string | null =>
  p.trackers.length === 0 && p.forge === null
    ? 'This project has no tracker and no forge. Edit the project to add one.'
    : null;

/** The list of one state: loading, error with "Try again", empty, or the rows. */
export function IssueRows({
  state,
  filtered,
  now,
  checked,
  open,
  canDrive,
  onCheck,
  onOpen,
  onRetry,
}: {
  readonly state: Load<readonly IssueSummary[]>;
  readonly filtered: boolean;
  readonly now: number;
  readonly checked: ReadonlySet<string>;
  readonly open: string | null;
  readonly canDrive: boolean;
  readonly onCheck: (key: string, on: boolean) => void;
  readonly onOpen: (key: string) => void;
  readonly onRetry: () => void;
}) {
  if (state.kind === 'loading') return <Loading text="Loading the issues…" />;
  if (state.kind === 'error') return <ErrorNotice text={state.text} onRetry={onRetry} />;
  if (state.value.length === 0) {
    return <p className="fd-empty">{filtered ? 'No issue matches.' : 'No issues.'}</p>;
  }
  return (
    <ul className="fd-int-list" aria-label="Issues">
      {state.value.map((i) => {
        const key = i.ref.key;
        return (
          <li
            key={key}
            className="fd-selbar"
            data-key={key}
            data-open={open === key ? 'true' : undefined}
          >
            {canDrive ? (
              <input
                type="checkbox"
                aria-label={`Select ${key}`}
                checked={checked.has(key)}
                onChange={(e) => onCheck(key, e.target.checked)}
              />
            ) : null}
            <button type="button" className="fd-int-row" onClick={() => onOpen(key)}>
              <span className="fd-int-key">{key}</span>
              <span className="fd-int-title">{i.ref.title}</span>
              <span className="fd-badge" data-tone="info">
                {i.status}
              </span>
              <span className="fd-int-meta">{i.assignee ?? 'Unassigned'}</span>
              <span className="fd-int-meta">{relativeAge(i.updated_at, now)}</span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

export function IssuesTab({
  project,
  actions,
  stations,
  now,
  initial,
  initialOpen = null,
  initialChecked = [],
}: Props) {
  const [search, setSearch] = useState('');
  const [query, setQuery] = useState('');
  const [mine, setMine] = useState(false);
  const [source, setSource] = useState<string | undefined>(undefined);
  const [open, setOpen] = useState<string | null>(initialOpen);
  const [checked, setChecked] = useState<ReadonlySet<string>>(new Set(initialChecked));
  const [starting, setStarting] = useState<readonly string[] | null>(null);
  // The query goes to the deck after a pause, not on every key.
  useEffect(() => {
    const timer = setTimeout(() => setQuery(search), SEARCH_MS);
    return () => clearTimeout(timer);
  }, [search]);
  const missing = noSourceText(project);
  const { state, reload } = useLoad<readonly IssueSummary[]>(
    () =>
      missing === null
        ? actions.issues(project.project_id, { source, query, mine })
        : Promise.resolve([]),
    `${project.project_id}|${source ?? ''}|${query}|${mine}`,
    initial,
  );
  const several = project.trackers.length >= 2;
  const picked = [...checked];
  return (
    <section className="fd-int-tab" aria-label="Issues">
      {missing === null ? null : <p className="fd-note">{missing}</p>}
      <form
        className="fd-int-filters"
        aria-label="Filter the issues"
        onSubmit={(e) => e.preventDefault()}
      >
        <label className="fd-int-search">
          <span className="fd-vh">Search the issues</span>
          <input
            type="search"
            aria-label="Search issues"
            placeholder="Search issues"
            autoComplete="off"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
        </label>
        <label className="fd-int-check">
          <input type="checkbox" checked={mine} onChange={(e) => setMine(e.target.checked)} />
          Mine
        </label>
        {several ? (
          <label>
            Tracker
            <select
              value={source ?? ''}
              onChange={(e) => setSource(e.target.value === '' ? undefined : e.target.value)}
            >
              {project.trackers.map((t, i) => (
                <option key={`${t.tracker}/${t.key}`} value={i === 0 ? '' : t.tracker}>
                  {t.kind} {t.key}
                </option>
              ))}
            </select>
          </label>
        ) : null}
        <button type="button" className="fd-ghost fd-sm" onClick={reload}>
          <Icon of={RefreshCw} size={14} /> Refresh
        </button>
      </form>
      {picked.length === 0 || !actions.canDrive ? null : (
        <div className="fd-int-bar" role="region" aria-label="Selected issues">
          <button type="button" className="fd-primary" onClick={() => setStarting(picked)}>
            Start sessions ({picked.length})
          </button>
          <button type="button" className="fd-ghost fd-sm" onClick={() => setChecked(new Set())}>
            Clear
          </button>
        </div>
      )}
      <div className="fd-int-split" data-open={open === null ? undefined : 'true'}>
        <div className="fd-int-main">
          <IssueRows
            state={state}
            filtered={query.trim() !== '' || mine}
            now={now}
            checked={checked}
            open={open}
            canDrive={actions.canDrive}
            onCheck={(key, on) =>
              setChecked((cur) => {
                const next = new Set(cur);
                if (on) next.add(key);
                else next.delete(key);
                return next;
              })
            }
            onOpen={setOpen}
            onRetry={reload}
          />
        </div>
        {open === null ? null : (
          <IssuePreview
            key={open}
            project={project}
            actions={actions}
            stations={stations}
            keyOf={open}
            source={source}
            now={now}
            onClose={() => setOpen(null)}
            onStart={(key) => setStarting([key])}
          />
        )}
      </div>
      {starting === null ? null : (
        <StartDialog
          project={project}
          actions={actions}
          stations={stations}
          issues={starting}
          onClose={() => setStarting(null)}
        />
      )}
    </section>
  );
}
