// R3 (DL-164): the project calls and the project list reach the pages, the Sidebar and the fleet
// card through one context, so that no prop crosses the shell.
import { createContext, type ReactNode, useCallback, useContext, useEffect, useState } from 'react';
import type { Project } from '../gen';
import type { IntegrationActions } from './actions';
import { message } from './useLoad';

export type IntegrationsEnv = {
  readonly actions: IntegrationActions;
  /** The projects; `null` until the first read, and always for a login without the admin scope. */
  readonly projects: readonly Project[] | null;
  /** Why the list did not load. */
  readonly error: string | null;
  /** Reads the list again (after a save or a delete). */
  readonly reload: () => void;
};

const Context = createContext<IntegrationsEnv | undefined>(undefined);

/** The calls and the projects, or `undefined` when the board has no deck. */
export const useIntegrations = (): IntegrationsEnv | undefined => useContext(Context);

/** The projects of the board, or none. */
export const useProjects = (): readonly Project[] => useContext(Context)?.projects ?? [];

/** Reads the project list once (the deck keeps it for the owner only) and shares it. */
export function IntegrationsProvider({
  actions,
  initial = null,
  children,
}: {
  readonly actions: IntegrationActions;
  /** The list at the first render (tests). */
  readonly initial?: readonly Project[] | null;
  readonly children: ReactNode;
}) {
  const [projects, setProjects] = useState<readonly Project[] | null>(initial);
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
  const reload = useCallback(() => setAgain((n) => n + 1), []);
  return (
    <Context.Provider value={{ actions, projects, error, reload }}>{children}</Context.Provider>
  );
}
