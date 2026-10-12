import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { takeLoginLink } from './runtime/api';
import { browserEnvironment, Connection } from './runtime/connection';
import { RuntimeApp } from './runtime/RuntimeApp';
import '@xterm/xterm/css/xterm.css';
import './styles/tokens.css';
import './styles/app.css';
import './styles/layout.css';
import './styles/runtime.css';
import './styles/session.css';
import './styles/spawn.css';
import './styles/chat.css';
import './styles/controls.css';
import './styles/terminal.css';

const root = document.getElementById('root');
if (root === null) {
  throw new Error('index.html has no #root element');
}
// The page, not a React effect, owns the runtime. StrictMode cannot redeem
// the one-time login link twice or create two socket retry loops.
const token = takeLoginLink(window.location, window.history);
const connection = new Connection(browserEnvironment());
connection.start(token);
createRoot(root).render(
  <StrictMode>
    <RuntimeApp connection={connection} />
  </StrictMode>,
);
