import { describe, expect, test } from 'bun:test';
import type { DeviceLink, DeviceLinkEntry } from '../gen';
import {
  countdown,
  defaultLinkIndex,
  deviceLinkAction,
  fingerprintGroups,
  needsTrustedCertificate,
} from './deviceLink';

const SHA = 'ab12cd34ef567890aabbccddeeff00112233445566778899aabbccddeeff0011';

const entry = (
  label: string,
  url: string,
  trust: DeviceLinkEntry['trust'] = 'self_signed',
): DeviceLinkEntry => ({ label, url, certificate_sha256: SHA, trust });

const link: DeviceLink = {
  links: [
    entry('This computer', 'https://localhost:7420/#login=t'),
    entry('Network 192.168.1.5', 'https://192.168.1.5:7420/#login=t'),
  ],
  expires_at: 1_700_000_300_000,
  certificate_sha256: 'ab12cd34ef567890aabbccddeeff00112233445566778899aabbccddeeff0011',
  self_signed: true,
};

describe('deviceLinkAction', () => {
  test('a good reply is the link, and the call is a POST to the device-link path', async () => {
    const calls: [string, unknown][] = [];
    const run = deviceLinkAction(async (path, body) => {
      calls.push([path, body]);
      return link;
    });
    expect(await run()).toEqual({ ok: true, link });
    expect(calls).toEqual([['/api/auth/device-link', {}]]);
  });

  test('a reply that has the wrong shape is refused', async () => {
    const bad: unknown[] = [
      null,
      [],
      {},
      { ...link, links: [] },
      { ...link, links: [{ label: 'x' }] },
      { ...link, links: [{ label: 'x', url: 'u', certificate_sha256: SHA, trust: 'maybe' }] },
      { ...link, links: [{ label: 'x', url: 'u', trust: 'trusted' }] },
      { ...link, links: 'x' },
      { ...link, expires_at: '1' },
      { ...link, certificate_sha256: 1 },
      { ...link, self_signed: 'yes' },
    ];
    for (const reply of bad) {
      const result = await deviceLinkAction(async () => reply)();
      expect(result).toEqual({
        ok: false,
        text: 'The deck gave an answer that the board cannot read.',
      });
    }
  });

  test('a thrown error is its message', async () => {
    const run = deviceLinkAction(async () => {
      throw new Error('Wait a minute before you add another device.');
    });
    expect(await run()).toEqual({
      ok: false,
      text: 'Wait a minute before you add another device.',
    });
    const odd = deviceLinkAction(async () => {
      throw 'x';
    });
    expect(await odd()).toEqual({ ok: false, text: 'The request failed.' });
  });
});

describe('helpers', () => {
  test('fingerprintGroups shows 16 hex characters in groups of 4, upper case', () => {
    expect(fingerprintGroups(link.certificate_sha256)).toBe('AB12 CD34 EF56 7890');
    expect(fingerprintGroups('AB:12:cd:34:ef:56:78:90:11')).toBe('AB12 CD34 EF56 7890');
    expect(fingerprintGroups('abcd')).toBe('ABCD');
    expect(fingerprintGroups('')).toBe('');
  });

  test('defaultLinkIndex skips "This computer" and falls back to the first', () => {
    const l = (label: string) => entry(label, `https://${label}/`);
    expect(defaultLinkIndex([l('This computer'), l('Network 1'), l('Tailnet d')])).toBe(1);
    expect(defaultLinkIndex([l('Network 1'), l('This computer')])).toBe(0);
    expect(defaultLinkIndex([l('This computer')])).toBe(0);
    expect(defaultLinkIndex([])).toBe(0);
  });

  test('defaultLinkIndex prefers the first trusted entry (DL-190)', () => {
    const l = (label: string, trust: DeviceLinkEntry['trust']) =>
      entry(label, `https://${label}/`, trust);
    const links = [
      l('This computer', 'self_signed'),
      l('Network 1', 'self_signed'),
      l('Tailnet d', 'trusted'),
      l('Tailnet e', 'trusted'),
    ];
    expect(defaultLinkIndex(links)).toBe(2);
    expect(defaultLinkIndex([l('This computer', 'trusted'), l('Network 1', 'unknown')])).toBe(0);
  });

  test('needsTrustedCertificate: a self-signed ts.net host only', () => {
    const e = (url: string, trust: DeviceLinkEntry['trust']) => entry('x', url, trust);
    const tailnet = 'https://Deck.Tail1.ts.net:7420/#login=t';
    expect(needsTrustedCertificate(e(tailnet, 'self_signed'))).toBe(true);
    expect(needsTrustedCertificate(e(tailnet, 'trusted'))).toBe(false);
    expect(needsTrustedCertificate(e(tailnet, 'unknown'))).toBe(false);
    expect(needsTrustedCertificate(e('https://192.168.1.5:7420/#login=t', 'self_signed'))).toBe(
      false,
    );
    expect(needsTrustedCertificate(e('not a url', 'self_signed'))).toBe(false);
  });

  test('countdown is m:ss, rounds up and stops at 0:00', () => {
    expect(countdown(300_000)).toBe('5:00');
    expect(countdown(299_001)).toBe('5:00');
    expect(countdown(65_000)).toBe('1:05');
    expect(countdown(9_000)).toBe('0:09');
    expect(countdown(500)).toBe('0:01');
    expect(countdown(0)).toBe('0:00');
    expect(countdown(-5)).toBe('0:00');
    expect(countdown(Number.NaN)).toBe('0:00');
  });
});
