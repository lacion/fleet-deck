import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { takeLoginLink } from './runtime/api';
import { browserEnvironment, Connection } from './runtime/connection';
import { RuntimeApp } from './runtime/RuntimeApp';
import { initDensity } from './ui/density';
import { initTheme } from './ui/theme';
import '@fontsource/ibm-plex-sans/latin-400.css';
import '@fontsource/ibm-plex-sans/latin-500.css';
import '@fontsource/ibm-plex-sans/latin-600.css';
import '@fontsource/ibm-plex-sans/latin-700.css';
import '@fontsource/ibm-plex-sans-condensed/latin-500.css';
import '@fontsource/ibm-plex-sans-condensed/latin-600.css';
import '@fontsource/ibm-plex-mono/latin-400.css';
import '@fontsource/ibm-plex-mono/latin-500.css';
import '@fontsource/ibm-plex-mono/latin-600.css';
import '@xterm/xterm/css/xterm.css';
import './styles/tokens.css';
import './styles/base.css';
import './styles/page.css';
import './styles/shell.css';
import './styles/home.css';
import './styles/session.css';
import './styles/chat.css';
import './styles/terminal.css';
import './styles/spawn.css';
import './styles/tailnet.css';
import './styles/views.css';
import './styles/artifacts.css';
import './styles/modelhelp.css';
import './styles/integrations.css';
import './styles/sharing.css';
import './styles/settings.css';

initTheme();
initDensity();
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
