import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DeviceLink } from '../gen';
import type { DeviceLinkResult } from '../runtime/deviceLink';
import { More } from './More';
import { Sidebar } from './Sidebar';
import type { Chrome } from './chrome';
import { DeviceLinkDialog } from './DeviceLinkDialog';

const SHA = 'ab12cd34ef567890aabbccddeeff00112233445566778899aabbccddeeff0011';
const LOGIN = 'https://192.168.1.5:7420/#login=abcdef0123456789abcdef0123456789';
const link = (over: Partial<DeviceLink> = {}): DeviceLink => ({
  links: [{ label: 'Network 192.168.1.5', url: LOGIN }],
  expires_at: 1_000_000 + 125_000,
  certificate_sha256: SHA,
  self_signed: false,
  ...over,
});
const ok = (l: DeviceLink): DeviceLinkResult => ({ ok: true, link: l });

const render = (initial?: DeviceLinkResult, now = 1_000_000) =>
  renderToStaticMarkup(
    <DeviceLinkDialog
      load={async () => ({ ok: false, text: 'unused' })}
      onClose={() => undefined}
      now={() => now}
      {...(initial === undefined ? {} : { initial })}
    />,
  );

describe('DeviceLinkDialog', () => {
  test('loading: one status line, a Close button and no link', () => {
    const html = render();
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-label="Add a device"');
    expect(html).toContain('<h2>Add a device</h2>');
    expect(html).toContain('Making a login link…');
    expect(html).toContain('Close');
    expect(html).not.toContain('QR code of the login link');
    expect(html).not.toContain('Try again');
  });

  test('error: the text as an alert and a Try again button', () => {
    const html = render({ ok: false, text: 'Wait a minute before you add another device.' });
    expect(html).toContain('role="alert"');
    expect(html).toContain('Wait a minute before you add another device.');
    expect(html).toContain('Try again');
    expect(html).not.toContain('Making a login link');
  });

  test('ready with one link: text, QR code, link field, copy button and countdown', () => {
    const html = render(ok(link()));
    expect(html).toContain(
      'Scan the code with the new device, or open the link there. The link works once.',
    );
    expect(html).toContain('role="img" aria-label="QR code of the login link"');
    expect(html).toContain('fill="#fff"');
    expect(html).toContain('fill="#000"');
    expect(html).toMatch(/viewBox="0 0 (\d+) \1"/);
    expect(html).toContain(`value="${LOGIN}"`);
    expect(html).toContain('readOnly');
    expect(html).toContain('Copy link');
    expect(html).toContain('Ends in 2:05');
    expect(html).not.toContain('<select');
    expect(html).not.toContain('own certificate');
  });

  test('ready with three links: a select of the labels, the first non-local one is the default', () => {
    const html = render(
      ok(
        link({
          links: [
            { label: 'This computer', url: 'https://localhost:7420/#login=t' },
            { label: 'Network 192.168.1.5', url: 'https://192.168.1.5:7420/#login=t' },
            { label: 'Tailnet deck.tail1.ts.net', url: 'https://deck.tail1.ts.net/#login=t' },
          ],
        }),
      ),
    );
    expect(html).toContain('<select');
    for (const label of ['This computer', 'Network 192.168.1.5', 'Tailnet deck.tail1.ts.net']) {
      expect(html).toContain(`>${label}</option>`);
    }
    expect(html).toContain('value="https://192.168.1.5:7420/#login=t"');
    expect(html).not.toContain('value="https://localhost:7420/#login=t"');
  });

  test('a self-signed board shows the start of the fingerprint', () => {
    const html = render(ok(link({ self_signed: true })));
    expect(html).toContain('The board uses its own certificate.');
    expect(html).toContain('check that the fingerprint starts with AB12 CD34 EF56 7890.');
  });

  test('expired: the link is gone and New link is offered', () => {
    const html = render(ok(link()), 1_000_000 + 125_000);
    expect(html).toContain('The link has ended.');
    expect(html).toContain('New link');
    expect(html).toContain('fd-primary');
    expect(html).not.toContain('QR code of the login link');
    expect(html).not.toContain(LOGIN);
  });

  test('a link that is too long for a QR code shows a note and no SVG', () => {
    const html = render(
      ok(link({ links: [{ label: 'Network', url: `https://h/#${'x'.repeat(600)}` }] })),
    );
    expect(html).toContain('The link is too long for a QR code; copy it instead.');
    expect(html).not.toContain('QR code of the login link');
    expect(html).toContain('Copy link');
  });
});

describe('the Add a device button', () => {
  const chrome = (addDevice?: Chrome['addDevice']): Chrome => ({
    label: 'Connected',
    freshness: 'live',
    onLogout: () => undefined,
    notices: [],
    ...(addDevice === undefined ? {} : { addDevice }),
  });
  const sidebar = (c: Chrome) =>
    renderToStaticMarkup(
      <Sidebar
        sessions={[]}
        stations={[]}
        route={{ name: 'home' }}
        terminalCount={0}
        onNavigate={() => undefined}
        onOpenTerminals={() => undefined}
        chrome={c}
      />,
    );
  const more = (c: Chrome) =>
    renderToStaticMarkup(<More stationCount={0} terminalCount={0} chrome={c} />);

  test('the sidebar and the More page show it only when the chrome can add a device', () => {
    const withAdd = chrome(async () => ({ ok: false, text: 'x' }));
    expect(sidebar(withAdd)).toContain('Add a device');
    expect(more(withAdd)).toContain('Add a device');
    expect(sidebar(chrome())).not.toContain('Add a device');
    expect(more(chrome())).not.toContain('Add a device');
  });

  test('the dialog is closed until the button is pressed', () => {
    const withAdd = chrome(async () => ({ ok: false, text: 'x' }));
    expect(sidebar(withAdd)).not.toContain('role="dialog"');
    expect(more(withAdd)).not.toContain('role="dialog"');
  });
});
