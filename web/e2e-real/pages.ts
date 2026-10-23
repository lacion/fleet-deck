// B17 §3.2: the local page servers of the browser test `wall-pages.spec.ts`. Two servers on 127.0.0.1
// with free ports:
// - P: `https` (a self-signed certificate) and `http` on a second port. Its page shows a text and a
//   full-size frame of N's page.
// - N: `https` on another port. Its page runs a script that tries `window.open('about:blank')` and
//   `top.location.href = 'https://example.invalid/'` in a `try`, and reports the results to N by a
//   `fetch` of `/report?open=<null|window>&top=<blocked|done>`.
// Each server records the `Cookie` header of every request (an empty text when there is none), so
// the test can prove that a page and its nested frame never get a cookie of the deck host (the ports
// differ, the host is the same, and cookies ignore the port).
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import {
  createServer as createHttp,
  type IncomingMessage,
  type Server,
  type ServerResponse,
} from 'node:http';
import { createServer as createHttps } from 'node:https';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { freePort } from './fleet';

/** The text of P's page: the test reads it inside the wall frame. */
export const P_TEXT = 'Page P says hello';
/** The text of N's page. */
export const N_TEXT = 'Page N is nested';

export type Report = { readonly open: string; readonly top: string };

/** One server: the addresses that it listens on and what it recorded. */
export type PageServer = {
  /** `https://127.0.0.1:<port>/` */
  readonly httpsUrl: string;
  /** `http://127.0.0.1:<port>/`, only for P. */
  readonly httpUrl: string | null;
  /** The `Cookie` header of every request, in order. */
  readonly cookies: string[];
  /** The reports that the page sent, in order (N only). */
  readonly reports: Report[];
  /** The number of requests that the server answered. */
  requests(): number;
};

export type Pages = {
  readonly p: PageServer;
  readonly n: PageServer;
  close(): Promise<void>;
};

/** A key and a certificate for 127.0.0.1, made by the `openssl` command line (one day). */
function selfSigned(dir: string): { key: string; cert: string } {
  const key = join(dir, 'key.pem');
  const cert = join(dir, 'cert.pem');
  execFileSync(
    'openssl',
    [
      'req',
      '-x509',
      '-newkey',
      'ec',
      '-pkeyopt',
      'ec_paramgen_curve:prime256v1',
      '-nodes',
      '-keyout',
      key,
      '-out',
      cert,
      '-days',
      '1',
      '-subj',
      '/CN=127.0.0.1',
      '-addext',
      'subjectAltName=IP:127.0.0.1',
    ],
    { stdio: 'ignore' },
  );
  return { key: readFileSync(key, 'utf8'), cert: readFileSync(cert, 'utf8') };
}

const pageP = (nUrl: string) => `<!doctype html>
<meta charset="utf-8"><title>Page P</title>
<body style="margin:0"><p id="p-text">${P_TEXT}</p>
<iframe title="nested" src="${nUrl}" style="width:100%;height:60vh;border:0"></iframe>`;

/** N's script. The report is sent when the page is still alive after 1.5 s: a top navigation that
 * went through would have replaced the wall, and no report would arrive. A thrown error is a block. */
const pageN = `<!doctype html>
<meta charset="utf-8"><title>Page N</title>
<body><p id="n-text">${N_TEXT}</p>
<script>
  let open = 'window';
  try {
    const w = window.open('about:blank');
    open = w === null ? 'null' : 'window';
    if (w !== null) w.close();
  } catch (e) {
    open = 'null';
  }
  let top = 'done';
  try {
    window.top.location.href = 'https://example.invalid/';
  } catch (e) {
    top = 'blocked';
  }
  setTimeout(() => {
    if (top === 'done') top = 'blocked';
    fetch('/report?open=' + open + '&top=' + top).catch(() => {});
  }, 1500);
</script>`;

type Handler = (req: IncomingMessage, res: ServerResponse) => void;

function recorder(
  state: { cookies: string[]; reports: Report[]; count: number },
  page: () => string,
): Handler {
  return (req, res) => {
    state.count += 1;
    state.cookies.push(req.headers.cookie ?? '');
    const url = new URL(req.url ?? '/', 'http://127.0.0.1');
    if (url.pathname === '/report') {
      state.reports.push({
        open: url.searchParams.get('open') ?? '',
        top: url.searchParams.get('top') ?? '',
      });
      res.writeHead(204).end();
      return;
    }
    if (url.pathname !== '/') {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
    res.end(page());
  };
}

function listen(server: Server, port: number): Promise<void> {
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', () => resolve());
  });
}

const closeAll = (server: Server): Promise<void> =>
  new Promise((resolve) => {
    server.closeAllConnections();
    server.close(() => resolve());
  });

/** Starts P (https and http) and N (https). */
export async function startPages(): Promise<Pages> {
  const dir = mkdtempSync(join(tmpdir(), 'fd-pages-'));
  const tls = selfSigned(dir);
  const [pHttps, pHttp, nHttps] = [await freePort(), await freePort(), await freePort()];
  const nUrl = `https://127.0.0.1:${nHttps}/`;
  const pState = { cookies: [] as string[], reports: [] as Report[], count: 0 };
  const nState = { cookies: [] as string[], reports: [] as Report[], count: 0 };
  const servers = [
    createHttps(
      tls,
      recorder(pState, () => pageP(nUrl)),
    ),
    createHttp(recorder(pState, () => pageP(nUrl))),
    createHttps(
      tls,
      recorder(nState, () => pageN),
    ),
  ];
  await listen(servers[0] as Server, pHttps);
  await listen(servers[1] as Server, pHttp);
  await listen(servers[2] as Server, nHttps);
  return {
    p: {
      httpsUrl: `https://127.0.0.1:${pHttps}/`,
      httpUrl: `http://127.0.0.1:${pHttp}/`,
      cookies: pState.cookies,
      reports: pState.reports,
      requests: () => pState.count,
    },
    n: {
      httpsUrl: nUrl,
      httpUrl: null,
      cookies: nState.cookies,
      reports: nState.reports,
      requests: () => nState.count,
    },
    close: async () => {
      await Promise.all(servers.map((s) => closeAll(s as Server)));
      rmSync(dir, { recursive: true, force: true });
    },
  };
}
