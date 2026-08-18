import { Component } from 'react';
import type { ErrorInfo, ReactNode } from 'react';

// The board renders one human-attention rail from server-persisted question
// rows, each shaped from an UNTRUSTED hook payload. If rendering one row throws
// (a malformed payload questionView() can't shape), React unmounts the whole
// tree — the entire board white-screens — and because the poison row survives
// every reload, it stays white-screened. This boundary contains a descendant
// render throw to the subtree it wraps: the caller supplies a `fallback` that
// renders in the failed subtree's place, and everything else keeps working.
//
// Error boundaries must be class components — getDerivedStateFromError /
// componentDidCatch have no hook equivalent.
interface Props {
  children: ReactNode;
  // Rendered in place of `children` after a descendant render throws. Receives
  // the caught error so the fallback can stay honest about what failed.
  fallback: (err: Error) => ReactNode;
}
interface State {
  err: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  override state: State = { err: null };

  static getDerivedStateFromError(err: unknown): State {
    return { err: err instanceof Error ? err : new Error(String(err)) };
  }

  override componentDidCatch(err: unknown, info: ErrorInfo): void {
    // Best-effort breadcrumb only; never rethrow from here.
    console.error('[fd] a UI subtree failed to render', err, info.componentStack);
  }

  override render(): ReactNode {
    const { err } = this.state;
    if (err) return this.props.fallback(err);
    return this.props.children;
  }
}
