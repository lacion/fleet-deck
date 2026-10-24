import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { EXPIRIES, ShareDialog, ShareList, ShareResult } from './ShareDialog';
import { fakeActions, NOW, share } from './fixtures';

const dialog = (extra: Partial<Parameters<typeof ShareDialog>[0]> = {}) =>
  renderToStaticMarkup(
    <ShareDialog
      station="station-a"
      id="a1"
      latest={3}
      actions={fakeActions()}
      initialShares={[]}
      onClose={() => {}}
      now={NOW}
      {...extra}
    />,
  );

describe('share dialog', () => {
  test('the form: version, expiry and "Make a link"', () => {
    const html = dialog();
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-label="Share the artifact"');
    expect(html).toContain('Latest (follows new versions)');
    expect(html).toContain('Version 3');
    expect(html).toContain('Version 1');
    for (const label of ['1 day', '7 days', '30 days', '1 year (the longest)']) {
      expect(html).toContain(label);
    }
    expect(html).toContain('Make a link');
    expect(html).toContain('No share links yet.');
    expect(html).not.toContain('QR code');
  });

  test('the expiry choices stay inside the deck limit of 1 to 365 days', () => {
    expect(EXPIRIES.map((x) => x.days)).toEqual([1, 7, 30, 365]);
  });

  test('a new link: the URL with Copy, the QR code and the warning', () => {
    const html = dialog({
      initialLink: {
        url: 'https://deck.test/a/0123abcd/',
        share_id: 'sh1',
        version: null,
        expires_at: NOW + 7 * 86_400_000,
      },
    });
    expect(html).toContain('value="https://deck.test/a/0123abcd/"');
    expect(html).toContain('Copy link');
    expect(html).toContain('aria-label="QR code of the share link"');
    expect(html).toContain('fill="#fff"');
    expect(html).toContain('Anyone with this link can open it.');
    expect(html).toContain('Always the latest version');
  });

  test('a fixed version shows its number in the result', () => {
    const html = renderToStaticMarkup(
      <ShareResult link={{ url: 'https://d/a/t/', share_id: 's', version: 2, expires_at: NOW }} />,
    );
    expect(html).toContain('Version 2');
  });

  test('an error is an alert', () => {
    const html = dialog({ initialError: 'only the owner’s board login makes share links' });
    expect(html).toContain('role="alert"');
    expect(html).toContain('only the owner’s board login makes share links');
  });

  test('the list: live shares have Revoke; revoked and expired ones have a badge', () => {
    const html = renderToStaticMarkup(
      <ShareList
        shares={[
          share('live', { views: 1 }),
          share('gone', { revoked: true, version: 2 }),
          share('old', { expires_at: NOW - 1000 }),
        ]}
        now={NOW}
        busy={null}
        onRevoke={() => {}}
      />,
    );
    expect(html.match(/>Revoke</g)?.length).toBe(1);
    expect(html).toContain('data-state="live"');
    expect(html).toContain('>Revoked<');
    expect(html).toContain('>Expired<');
    expect(html).toContain('Version 2');
    expect(html).toContain('1 view');
    expect(html).toContain('3 views');
    expect(html).not.toContain('https://');
  });

  test('a revoke in progress disables the buttons', () => {
    const html = renderToStaticMarkup(
      <ShareList shares={[share('live')]} now={NOW} busy="live" onRevoke={() => {}} />,
    );
    expect(html).toContain('Revoking…');
    expect(html).toContain('disabled');
  });
});
