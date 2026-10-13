// R3 (DL-164): the project calls and the project list reach the pages, the Sidebar and the fleet
// card through one context, so that no prop crosses the shell.
import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from 'react';
import type { PrSummary, Project } from '../gen';
import type { IntegrationActions } from './actions';
import { allPrs } from './model';
import { message } from './useLoad';

export type IntegrationsEnv = {
  readonly actions: IntegrationActions;
  /** The projects; `null` until the first read, and always for a login without the admin scope. */
  readonly projects: readonly Project[] | null;
  /** The pull requests of each project with a forge, from all four groups. */
  readonly prs: PrMap;
  /** Why the list did not load. */
  readonly error: string | null;
  /** Reads the list again (after a save or a delete). */
  readonly reload: () => void;
};

/** The pull requests by project id. */
export type PrMap = ReadonlyMap<string, readonly PrSummary[]>;

const NO_PRS: PrMap = new Map();

/** How often the pull requests are read again while the page is visible. */
export const PRS_EVERY_MS = 10 * 60_000;

/**
 * Reads the pull requests of every project with a forge. A failed read keeps the previous list of
 * that project; nothing shows the error.
 */
export async function readPrs(
  actions: IntegrationActions,
  projects: readonly Project[],
  previous: PrMap,
): Promise<PrMap> {
  const forged = projects.filter((p) => p.forge !== null);
  const lists = await Promise.all(
    forged.map((p) =>
      actions.prs(p.project_id).then(
        (groups) => allPrs(groups),
        () => previous.get(p.project_id) ?? [],
      ),
    ),
  );
  return new Map(forged.map((p, i) => [p.project_id, lists[i] ?? []]));
}

const Context = createContext<IntegrationsEnv | undefined>(undefined);

/** The calls and the projects, or `undefined` when the board has no deck. */
export const useIntegrations = (): IntegrationsEnv | undefined => useContext(Context);

/** The projects of the board, or none. */
export const useProjects = (): readonly Project[] => useContext(Context)?.projects ?? [];

/** The pull requests of the board, by project id. */
export const usePrs = (): PrMap => useContext(Context)?.prs ?? NO_PRS;

/** Reads the project list once (the deck keeps it for the owner only) and shares it. */
export function IntegrationsProvider({
  actions,
  initial = null,
  initialPrs = NO_PRS,
  children,
}: {
  readonly actions: IntegrationActions;
  /** The list at the first render (tests). */
  readonly initial?: readonly Project[] | null;
  /** The pull requests at the first render (tests). */
  readonly initialPrs?: PrMap;
  readonly children: ReactNode;
}) {
  const [projects, setProjects] = useState<readonly Project[] | null>(initial);
  const [prs, setPrs] = useState<PrMap>(initialPrs);
  const prsRef = useRef<PrMap>(initialPrs);
  const [error, setError] = useState<string | null>(null);
  const [again, setAgain] = useState(0);
  const admin = actions.canAdmin;
  useEffect(() => {
    if (!admin || again < 0) return undefined;
    let live = true;
    actions.projects().then(
      (list) => {
        if (!live) return;
        setProjects(list);
        setError(null);
      },
      (e: unknown) => live && setError(message(e)),
    );
    return () => {
      live = false;
    };
  }, [actions, admin, again]);
  // The pull requests: after the projects load (also after `reload`), then every 10 minutes while
  // the page is visible.
  useEffect(() => {
    if (!admin || projects === null) return undefined;
    let live = true;
    const load = () => {
      readPrs(actions, projects, prsRef.current).then((next) => {
        if (!live) return;
        prsRef.current = next;
        setPrs(next);
      });
    };
    load();
    const timer = setInterval(() => {
      if (document.visibilityState !== 'hidden') load();
    }, PRS_EVERY_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [actions, admin, projects]);
  const reload = useCallback(() => setAgain((n) => n + 1), []);
  return (
    <Context.Provider value={{ actions, projects, prs, error, reload }}>
      {children}
    </Context.Provider>
  );
}
