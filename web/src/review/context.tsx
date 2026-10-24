// DL-179: the review calls reach the session view through one context, so that no prop crosses
// the session route (the same shape as `artifacts/context.tsx`).
import { createContext, useContext } from 'react';
import type { ReviewActions } from './api';

const Context = createContext<ReviewActions | undefined>(undefined);

export const ReviewProvider = Context.Provider;

/** The review calls, or `undefined` when the board has no deck. */
export const useReviewActions = (): ReviewActions | undefined => useContext(Context);
