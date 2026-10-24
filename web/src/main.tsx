import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { DisplayRuntime } from './display/DisplayRuntime';
import {
  displayEnvironment,
  isDisplayPath,
  redeemDisplayLink,
  takeDisplayLink,
} from './display/api';
import { appActions } from './apps/actions';
import { registerAppActions } from './apps/env';
import { takeLoginLink } from './runtime/api';
import { browserEnvironment, Connection } from './runtime/connection';
import { RuntimeApp } from './runtime/RuntimeApp';
import { deckHttp } from './settings/http';
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
import './styles/apps.css';

/** The longest wait for one app call: the deck waits 70 s for the station (design §4.7). */
const APP_CALL_WAIT_MS = 75_000;

initTheme();
initDensity();
// The last kind of input on `<html data-modality>`: `pointer` after a press of a mouse, a pen or a
// finger, `keyboard` after a key. Chromium matches :focus-visible on a <select> picked with the
// pointer, so the quiet ghost select (base.css `select.fd-ghost`, DESIGN-SYSTEM §3) reads this to
// keep its focus edge for the keyboard only. A key press brings the edge back at once.
for (const [type, modality] of [
  ['pointerdown', 'pointer'],
  ['keydown', 'keyboard'],
] as const) {
  document.addEventListener(
    type,
    () => {
      document.documentElement.dataset['modality'] = modality;
    },
    { capture: true, passive: true },
  );
}
// A theme change is one step (base.css `:root[data-theme-switching]`, DESIGN-SYSTEM §2.5). Every
// write of `<html data-theme>` (the Appearance choice, the System scheme at sunset, forced colors,
// an Omarchy palette: `ui/theme.ts`, `ui/palette.ts`) stops the color transitions for two frames,
// so that no button, tab or segment fades for 120ms through the old theme. The observer runs
// before the next style update, and a transition that a style read had started is cancelled.
const html = document.documentElement;
let themeWrites = 0;
const themeWatch = new MutationObserver(() => {
  const write = ++themeWrites;
  html.dataset['themeSwitching'] = '';
  requestAnimationFrame(() =>
    requestAnimationFrame(() => {
      if (write === themeWrites) delete html.dataset['themeSwitching'];
    }),
  );
});
themeWatch.observe(html, { attributeFilter: ['data-theme'] });
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
  // B15: the app calls of the chat go through the deck transport (the cookie and the CSRF token).
  registerAppActions(
    appActions(
      deckHttp(
        (path, init) => fetch(path, init),
        () => connection.getSnapshot().login?.csrf ?? null,
        APP_CALL_WAIT_MS,
      ),
    ),
  );
  createRoot(root).render(
    <StrictMode>
      <RuntimeApp connection={connection} />
    </StrictMode>,
  );
}
