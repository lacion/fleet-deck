import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './tokens.css';
import './app.css';
import App from './App.tsx';
import { initToken } from './token.ts';
import { installStaleChunkGuard } from './staleChunks.ts';

// Before the first paint (and so before the first request): adopt the token
// from ?t=… and scrub it out of the address bar.
initToken();

// BUG-083 — after a daemon upgrade the hashed terminal chunks this entry
// references may be gone; reload once on a failed lazy preload instead of
// unmounting the tree.
installStaleChunkGuard(window);

createRoot(document.getElementById('root') as unknown as HTMLElement).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
