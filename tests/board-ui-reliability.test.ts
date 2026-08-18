import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import test from './helpers/harness-test.ts';
import { shareInfoForHref } from '../board/src/share.ts';
import { joinBrowsePath, normalizeBrowseRoot, relativeBrowsePath } from '../board/src/fsPath.ts';

const app = readFileSync(path.resolve('board/src/App.tsx'), 'utf8');
const hotkeys = readFileSync(path.resolve('board/src/hooks/useBoardHotkeys.ts'), 'utf8');
const fleetState = readFileSync(path.resolve('board/src/useFleetState.ts'), 'utf8');
const fleetConnection = readFileSync(path.resolve('board/src/fleetConnection.ts'), 'utf8');
const api = readFileSync(path.resolve('board/src/api.ts'), 'utf8');
const spawnForm = readFileSync(path.resolve('board/src/components/SpawnForm.tsx'), 'utf8');
const killConfirm = readFileSync(path.resolve('board/src/components/KillConfirm.tsx'), 'utf8');

test('Share recognizes a Coder/proxy origin and never leaks the board token', () => {
  const got = shareInfoForHref(
    'https://fleetdeck--workspace.example.dev/apps/fleet/?t=secret&view=board#session',
  );
  assert.equal(got.remote, true);
  assert.equal(got.proxied, true);
  assert.equal(got.url, 'https://fleetdeck--workspace.example.dev/apps/fleet/?view=board');
  assert.doesNotMatch(got.url, /secret|[?&]t=/);
});

test('Share recognizes the canonical Coder app route even on local Coder', () => {
  const got = shareInfoForHref(
    'http://127.0.0.1:3000/@ada/workspace.main/apps/fleetdeck/?t=secret',
  );
  assert.equal(got.remote, false, 'localhost itself is not a shareable remote URL');
  assert.equal(got.proxied, true, 'the Coder app route is still an authenticated proxy boundary');
  assert.doesNotMatch(got.url, /secret|[?&]t=/);
});

test('Share keeps every common loopback spelling local-only', () => {
  for (const href of [
    'http://localhost:4711/',
    'http://fleet.localhost:4711/',
    'http://127.0.0.2:4711/',
    'http://[::1]:4711/',
    'http://[::ffff:127.0.0.1]:4711/',
  ]) {
    assert.equal(shareInfoForHref(href).remote, false, href);
    assert.equal(shareInfoForHref(href).proxied, false, href);
  }
  assert.deepEqual(shareInfoForHref('not a URL'), { remote: false, proxied: false, url: '' });
});

test('file viewer joins the filesystem root without a double slash', () => {
  assert.equal(joinBrowsePath('/', 'child/file.ts'), '/child/file.ts');
  assert.equal(joinBrowsePath('/workspace/', '/src/index.ts'), '/workspace/src/index.ts');
  assert.equal(joinBrowsePath('/workspace', ''), '/workspace');
  assert.equal(normalizeBrowseRoot('/workspace///'), '/workspace');
  assert.equal(relativeBrowsePath('/workspace/', '/workspace/src/index.ts'), 'src/index.ts');
  assert.equal(relativeBrowsePath('/', '/tmp/project'), 'tmp/project');
  assert.equal(relativeBrowsePath('/workspace', '/workspace-old/file.ts'), null);
});

test('an idle healthy WebSocket is not recycled by an invented frame heartbeat', () => {
  assert.doesNotMatch(fleetState, /frameWatchdog|lastFrameAt/);
  assert.doesNotMatch(fleetConnection, /frameWatchdog|lastFrameAt/);
  assert.match(fleetConnection, /queuePoll\(st\.socketOpen \? 15_000 : 3_000\)/);
  assert.match(fleetConnection, /st\.healthFailures >= 2/);
});

test('the session drawer blocks global answer hotkeys behind its modal scrim', () => {
  assert.match(app, /drawerOpen:\s*drawerOpenRef/);
  assert.match(hotkeys, /blockingOverlayOpen\([\s\S]*?drawerOpen,[\s\S]*?\]\)/);
});

test('lazy terminal surfaces have visible, dismissible loading and failure states', () => {
  assert.doesNotMatch(app, /<React\.Suspense fallback=\{null\}>/);
  assert.match(app, /<TerminalFallback onClose=\{closeTerm\}/);
  assert.match(app, /<TerminalFallback[\s\S]*?grid[\s\S]*?setGrid\(null\)/);
  assert.match(app, /<ErrorBoundary/);
  assert.match(app, /window\.location\.reload\(\)/);
});

test('repo spawning checks exact Git access and explains Coder auth without owning tokens', () => {
  assert.match(spawnForm, /preflightRepo/);
  assert.match(spawnForm, /auth_label/);
  assert.match(spawnForm, /Use HTTPS instead/);
  assert.match(spawnForm, /Copy SSH key/);
  assert.match(spawnForm, /FleetDeck never stores Git tokens/);
  assert.match(spawnForm, /Git diagnostic/);
  const submit = spawnForm.slice(spawnForm.indexOf('const submit = async'));
  assert.ok(
    submit.indexOf('await checkRepoAccess()') < submit.indexOf('await persistSetupDefault()'),
    'Spawn proves Git access before any settings write or spawn request',
  );
  assert.match(spawnForm, /checking repository access before creating a session/);
  assert.match(spawnForm, /Checking access…/);
  assert.match(api, /post\('\/api\/repos\/preflight', body, 65_000\)/);
});

test('a held Claude trust dialog opens the owned terminal instead of looking like a spawn hang', () => {
  assert.match(spawnForm, /attention === 'folder-trust'/);
  assert.match(spawnForm, /approval required — trust this new folder once/);
  assert.match(spawnForm, /onOpenTerminal\(sess\)/);
  assert.match(
    app,
    /onOpenTerminal=\{\(session\) => \{[\s\S]*?setSpawnForm\(null\);[\s\S]*?openTerm\(session\)/,
  );
});

test('a provisioning card offers clone cancellation rather than a fake pane kill', () => {
  assert.match(killConfirm, /Cancel the repository clone/);
  assert.match(killConfirm, /stops Git and its credential helpers/);
  assert.match(killConfirm, /Cancel clone/);
});
