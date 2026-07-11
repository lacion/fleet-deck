import React from 'react';
import { createRoot } from 'react-dom/client';
import './tokens.css';
import './app.css';
import App from './App.jsx';
import { initToken } from './token.js';

// Before the first paint (and so before the first request): adopt the token
// from ?t=… and scrub it out of the address bar.
initToken();

createRoot(document.getElementById('root')).render(<App />);
