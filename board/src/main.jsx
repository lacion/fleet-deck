import React from 'react';
import { createRoot } from 'react-dom/client';
import './tokens.css';
import './app.css';
import App from './App.jsx';
import { initToken } from './token.js';
import { installStaleChunkGuard } from './staleChunks.js';

// Before the first paint (and so before the first request): adopt the token
// from ?t=… and scrub it out of the address bar.
initToken();

// BUG-083 — after a daemon upgrade the hashed terminal chunks this entry
// references may be gone; reload once on a failed lazy preload instead of
// unmounting the tree.
installStaleChunkGuard(window);

createRoot(document.getElementById('root')).render(<App />);
