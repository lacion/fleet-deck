// B15 MCP Apps (design docs/v2/design/mcp-apps.md §4.8, T8): the app row against the fake deck. The
// fake deck serves `/app/{token}` with the §4.7 CSP and the exact bootstrap, answers the app calls
// and serves `/a/{path}` pages with the artifact CSP.
import { expect, type Page, test } from '@playwright/test';
import type { DeckSession, Event } from '../src/gen';
import { summary } from '../src/testing/summary';
import { claudeCapabilities, envelopes } from '../src/testing/timeline';
import {
  type FakeApps,
  FakeDeck,
  fakeApps,
  installApps,
  logIn,
  openSession,
  stationView,
} from './fake-deck';

const ST = 'st-alpha';
const ID = 's1';
const APP = '01JDEMOAPP00000000000000000';

const session = (): DeckSession => ({
  station_id: ST,
  status: 'idle',
  summary: summary(ID, 'idle', 1, {
    title: 'Count things',
    capabilities: claudeCapabilities(),
    cwd: '/home/me/app',
  }),
});

const events: Event[] = [
  { kind: 'run.started', run_id: 'run-1', input_summary: 'show', origin: 'board' },
  { kind: 'turn.started', run_id: 'run-1', turn_id: 'turn-1', provider_refs: {} },
  {
    kind: 'tool.started',
    node_id: 'node-app',
    parent_node_id: null,
    tool_kind: 'mcp',
    title: 'mcp__demo__show_counter',
    input_summary: '{}',
    files: [],
    provider_refs: {},
  },
  { kind: 'tool.completed', node_id: 'node-app', status: 'ok', files: [], exit_code: null },
  {
    kind: 'tool.app',
    node_id: 'node-app',
    app_id: APP,
    server: 'demo',
    tool: 'show_counter',
    resource_uri: 'ui://demo/counter',
    prefers_border: true,
  },
];

/** A document that initializes with the host, then runs `extra` (it can set `#out` text). */
const doc = (extra: string) => `<!doctype html><html><body><p id="out"></p>
<script>
const out=(t)=>{document.getElementById("out").textContent=t};
const rpc=(method,params)=>new Promise(res=>{const id=1;const on=e=>{if(e.data&&e.data.id===id){window.removeEventListener("message",on);res(e.data)}};window.addEventListener("message",on);window.parent.postMessage({jsonrpc:"2.0",id,method,params},"*")});
rpc("ui/initialize",{protocolVersion:"2026-01-26",appInfo:{name:"t",version:"1"},appCapabilities:{}}).then(()=>{
window.parent.postMessage({jsonrpc:"2.0",method:"ui/notifications/initialized",params:{}},"*");
${extra}});
</script></body></html>`;

async function setUp(page: Page, prepare: (apps: FakeApps) => void = () => {}) {
  const deck = new FakeDeck();
  deck.sessions = [session()];
  deck.stations = [stationView(ST, 'Alpha')];
  await deck.install(page);
  const apps = fakeApps(deck, ST, ID, APP);
  prepare(apps);
  const gate = await installApps(page, apps);
  await logIn(page);
  deck.emit(ST, ID, envelopes(ID, events));
  await openSession(page, 'Count things');
  return { deck, apps, gate };
}

const frame = (page: Page) => page.frameLocator('iframe.fd-app-frame');

test('a button in the demo app calls bump after Allow and the counter text changes', async ({
  page,
}) => {
  const { apps } = await setUp(page);
  await expect(page.getByText('App from demo')).toBeVisible();
  await expect(frame(page).locator('#counter')).toHaveText('Count: 0');
  await frame(page).locator('#bump').click();
  const dialog = page.getByRole('dialog', { name: 'Allow this app action?' });
  await expect(dialog).toBeVisible();
  await dialog.getByRole('button', { name: 'Allow' }).click();
  await expect(frame(page).locator('#counter')).toHaveText('Count: 1');
  expect(apps.calls.map((c) => [c.request.name, c.confirmed])).toEqual([
    ['bump', false],
    ['bump', true],
  ]);
});

test('the app document cannot fetch /api (blocked by CSP)', async ({ page }) => {
  await setUp(page, (apps) => {
    apps.token = 'fetch';
    apps.documents['fetch'] = doc(
      `fetch("/api/auth/session").then(()=>out("fetch allowed"),()=>out("fetch blocked"));`,
    );
  });
  await expect(frame(page).locator('#out')).toHaveText('fetch blocked');
});

test('the app cannot read document.cookie of the board (opaque origin)', async ({
  page,
  baseURL,
}) => {
  await page.context().addCookies([{ name: 'board', value: 'secret', url: `${baseURL}/` }]);
  await setUp(page, (apps) => {
    apps.token = 'cookie';
    apps.documents['cookie'] = doc(
      // `location.origin` is the URL's origin; `self.origin` is the opaque origin of the sandbox.
      `out("cookie=[" + document.cookie + "] origin=" + self.origin);`,
    );
  });
  await expect(frame(page).locator('#out')).toHaveText('cookie=[] origin=null');
});

test('navigating the frame stops the bridge', async ({ page }) => {
  await setUp(page, (apps) => {
    apps.token = 'nav';
    apps.documents['nav'] = doc(`setTimeout(()=>{location.href="/a/plain/"},200);`);
    apps.pages['plain/'] = '<!doctype html><p>plain</p>';
  });
  await expect(page.getByText('The app left its page and was stopped.')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Reload app' })).toBeVisible();
  await expect(page.locator('iframe.fd-app-frame')).toHaveCount(0);
});

test('a replacement document gets no answer before its load event', async ({ page }) => {
  const { apps, gate } = await setUp(page, (apps) => {
    apps.token = 'replace';
    apps.documents['replace'] = doc(`location.href="/a/replace/";`);
    apps.pages['replace/'] = `<!doctype html><script>
function beacon(k){new Image().src="/a/beacon?"+k}
window.addEventListener("message",()=>beacon("window-answer"));
window.parent.postMessage({jsonrpc:"2.0",id:1,method:"tools/call",params:{name:"bump",arguments:{}}},"*");
const mc=new MessageChannel();mc.port1.onmessage=()=>beacon("port-answer");
window.parent.postMessage({"fleet-deck-app-hello":1},"*",[mc.port2]);
</script><img src="/a/slow.png">`;
  });
  // The forged hello closes the bridge while the slow image still holds the replacement's load.
  await expect(page.getByText('The app left its page and was stopped.')).toBeVisible();
  gate.release();
  await page.waitForTimeout(300);
  expect(apps.calls).toHaveLength(0);
  expect(apps.beacons.filter((b) => b.includes('answer'))).toEqual([]);
});
