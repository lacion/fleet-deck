import { appendFileSync, writeSync } from 'node:fs';

const rawPort = Number(process.argv[2]);
const rawEventFile = process.argv[3];
if (!Number.isInteger(rawPort) || rawPort < 1 || rawPort > 65_535 || !rawEventFile) {
  process.stderr.write('usage: fleetd.ts <port> <event-file>\n');
  process.exit(2);
  throw new Error('unreachable after usage failure');
}
const port = rawPort;
const eventFile = rawEventFile;

function record(event: string): void {
  appendFileSync(eventFile, `${JSON.stringify({ event, at: Date.now() })}\n`);
}

const server = Bun.serve({
  hostname: '127.0.0.1',
  port,
  fetch(request) {
    if (new URL(request.url).pathname === '/health') {
      record('health-request');
      return new Promise<Response>(() => undefined);
    }
    return new Response('not found', { status: 404 });
  },
});

let closing = false;
const close = (): void => {
  if (closing) return;
  closing = true;
  void server.stop(true).finally(() => process.exit(0));
};
process.on('SIGINT', close);
process.on('SIGTERM', close);

record('ready');
writeSync(process.stdout.fd, `${JSON.stringify({ event: 'ready', pid: process.pid, port })}\n`);
