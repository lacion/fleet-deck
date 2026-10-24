// B29: the deck read of the analytics page (design analytics.md, build answer A10 Q1). The board
// carries deck calls to pages through a context (as `sharing/context.tsx`), so `App` has no prop.
import { createContext, useContext } from 'react';

export type AnalyticsGet = (path: string) => Promise<unknown>;

const Context = createContext<AnalyticsGet | undefined>(undefined);

export const AnalyticsProvider = Context.Provider;
export const useAnalyticsGet = (): AnalyticsGet | undefined => useContext(Context);
