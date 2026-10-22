import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { DisplayRuntime } from './display/DisplayRuntime';
import {
  displayEnvironment,
  isDisplayPath,
  redeemDisplayLink,
  takeDisplayLink,
} from './display/api';
import { takeLoginLink } from './runtime/api';
import { browserEnvironment, Connection } from './runtime/connection';
import { RuntimeApp } from './runtime/RuntimeApp';
import { initDensity } from './ui/density';
import { initTheme } from './ui/theme';
import '@fontsource-variable/mona-sans/standard.css';
import '@fontsource-variable/jetbrains-mono';
import '@xterm/xterm/css/xterm.css';
import './styles/tokens.css';
import './styles/base.css';
import './styles/page.css';
import './styles/shell.css';
import './styles/stations.css';
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
import './styles/wall.css';
import './styles/displays.css';

initTheme();
initDensity();
const root = document.getElementById('root');
if (root === null) {
  throw new Error('index.html has no #root element');
}
// The page, not a React effect, owns the runtime. StrictMode cannot redeem
// the one-time login link twice or create two socket retry loops.
if (isDisplayPath(window.location.pathname)) {
  // R13 P2c: the wall display document (`/display`): a display login, never the owner's board.
  const link = takeDisplayLink(window.location, window.history);
  const environment = displayEnvironment();
  const connection = new Connection(environment);
  createRoot(root).render(
    <StrictMode>
      <DisplayRuntime connection={connection} />
    </StrictMode>,
  );
  void (async () => {
    if (link !== null && (await redeemDisplayLink(environment.fetch, link))) {
      window.history.replaceState(null, '', '/display#/wall');
    }
    connection.start();
  })();
} else {
  const token = takeLoginLink(window.location, window.history);
  const connection = new Connection(browserEnvironment());
  connection.start(token);
  createRoot(root).render(
    <StrictMode>
      <RuntimeApp connection={connection} />
    </StrictMode>,
  );
}
