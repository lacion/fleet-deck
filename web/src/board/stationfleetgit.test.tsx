// FG-6, FG-11, FG-12 (design §9.4, §14.3, §15.7): the traits line, the config drift report and the
// Taildrive section of the Stations page.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ConfigDigest, StationInfo, TaildriveShare } from '../gen';
import { IntegrationsProvider } from '../integrations/context';
import { fakeActions } from '../integrations/fixtures';
import { station } from '../testing/fleet';
import { StationsPanel } from './StationsPanel';
import {
  ConfigReferenceHeader,
  configText,
  driftLabels,
  StationConfigDrift,
} from './StationConfigDrift';
import {
  exposedText,
  shareNotes,
  StationTaildrive,
  TaildriveFact,
  taildriveFact,
} from './StationTaildrive';
import { StationTraits, traitsText } from './StationTraits';

const HASH = 'a'.repeat(64);
const digest = (extra: Partial<Record<string, string>> = {}, versions = {}): ConfigDigest => ({
  claude_version: '2.1.0',
  codex_version: '0.9.0',
  computed_at: 1,
  items: [
    { name: 'claude_md', hash: extra['claude_md'] ?? HASH },
    { name: 'claude_agents', hash: extra['claude_agents'] ?? HASH },
    { name: 'claude_skills', hash: extra['claude_skills'] ?? HASH },
    { name: 'codex_agents_md', hash: extra['codex_agents_md'] ?? HASH },
  ],
  ...versions,
});
const withConfig = (id: string, config: ConfigDigest | undefined) =>
  station(id, { info: { config } as unknown as StationInfo });

describe('the traits line', () => {
  test('os, arch, battery and labels', () => {
    expect(traitsText({ os: 'linux', arch: 'x86_64', labels: [] })).toBe('Linux · x86_64');
    // One separator for every part: each label reads as its own value, as a word.
    expect(
      traitsText({ os: 'macos', arch: 'aarch64', on_battery: true, labels: ['gpu', 'big-ram'] }),
    ).toBe('macOS · aarch64 · on battery · GPU · Big ram');
    expect(traitsText({ os: 'linux', arch: 'x86_64', on_battery: false, labels: ['gpu'] })).toBe(
      'Linux · x86_64 · GPU',
    );
    // The display name of the OS; an id without one stays as it is.
    expect(traitsText({ os: 'omarchy', arch: 'x86_64', labels: [] })).toBe('Omarchy · x86_64');
    expect(traitsText({ os: 'freebsd', arch: 'amd64', labels: [] })).toBe('freebsd · amd64');
  });

  test('an older station sends no traits: no line', () => {
    expect(renderToStaticMarkup(<StationTraits traits={undefined} />)).toBe('');
    expect(
      renderToStaticMarkup(<StationTraits traits={{ os: 'linux', arch: 'x', labels: [] }} />),
    ).toContain('Linux · x');
  });
});

describe('the config line', () => {
  const reference = withConfig('st-ref', digest());

  test('the reference, the same, the differences, and no line without a comparison', () => {
    expect(configText(reference, reference)).toBe('Config: Reference');
    expect(configText(withConfig('st-b', digest()), reference)).toBe('Config: Same');
    expect(
      configText(withConfig('st-b', digest({ claude_skills: 'b'.repeat(64) })), reference),
    ).toBe('Config: Differs: skills');
    // The labels follow the order of the report: versions, then the four items.
    expect(
      configText(
        withConfig(
          'st-b',
          digest(
            { claude_skills: 'b', claude_agents: 'c', codex_agents_md: 'd' },
            { codex_version: '1.0.0' },
          ),
        ),
        reference,
      ),
    ).toBe('Config: Differs: codex, agents, skills, AGENTS.md');
    // No "unknown" (DESIGN-SYSTEM §1.7): nothing to compare is no line.
    expect(configText(withConfig('st-c', undefined), reference)).toBeNull();
    expect(configText(withConfig('st-b', digest()), withConfig('st-ref', undefined))).toBeNull();
    expect(configText(withConfig('st-b', digest()), undefined)).toBeNull();
    expect(
      renderToStaticMarkup(
        <StationConfigDrift station={withConfig('st-c', undefined)} reference={reference} />,
      ),
    ).toBe('');
  });

  test('the version of a provider counts, and a missing version equals a missing version', () => {
    expect(driftLabels(digest(), digest({}, { claude_version: '2.2.0' }))).toEqual(['claude']);
    expect(
      driftLabels(
        digest({}, { claude_version: undefined }),
        digest({}, { claude_version: undefined }),
      ),
    ).toEqual([]);
  });

  test('the header menu says what it does, then names the reference', () => {
    const stations = [station('station-a'), station('st-b')];
    const none = renderToStaticMarkup(
      <ConfigReferenceHeader stations={stations} reference={undefined} canSet onSet={() => {}} />,
    );
    // A menu toggle (trailing chevron), not a dialog (no ellipsis).
    expect(none).toContain(
      '<summary class="fd-ghost fd-sm" data-variant="toggle">Compare configs</summary>',
    );
    expect(none).toContain('<details class="fd-menu">');
    expect(none).toContain('Pick a reference station for the config report.');
    const set = renderToStaticMarkup(
      <ConfigReferenceHeader stations={stations} reference={stations[0]} canSet onSet={() => {}} />,
    );
    expect(set).toContain(
      '<summary class="fd-ghost fd-sm" data-variant="toggle">Configs vs Alpha</summary>',
    );
    expect(set.match(/<button type="button">/g)?.length).toBe(2);
    // Without the admin scope: the reference as a plain line, and nothing before one exists.
    const locked = renderToStaticMarkup(
      <ConfigReferenceHeader
        stations={stations}
        reference={stations[0]}
        canSet={false}
        onSet={() => {}}
      />,
    );
    expect(locked).toBe('<span class="fd-note fd-station-ref">Configs vs Alpha</span>');
    expect(
      renderToStaticMarkup(
        <ConfigReferenceHeader
          stations={stations}
          reference={undefined}
          canSet={false}
          onSet={() => {}}
        />,
      ),
    ).toBe('');
  });
});

const share = (extra: Partial<TaildriveShare> = {}): TaildriveShare => ({
  name: 'out',
  path: '/srv/out',
  candidates: ['/srv/out'],
  as_user: 'ada',
  exposed: [],
  exposure_state: 'clear',
  path_state: 'ok',
  ...extra,
});

const noUser = (): TaildriveShare => {
  const { as_user: _gone, ...rest } = share({ name: 'web', path: '/srv/web' });
  return rest;
};

const drive = (td: StationInfo['taildrive']) =>
  renderToStaticMarkup(<StationTaildrive taildrive={td} />);

describe('the Taildrive section', () => {
  test('an ok report lists the shares with the user', () => {
    const html = drive({
      state: 'ok',
      shares: [share(), noUser()],
      checked_at: 1,
      inventory_revision: 1,
    });
    expect(html).toContain('Taildrive shares');
    expect(html).toContain('out · /srv/out · as ada');
    expect(html).toContain('web · /srv/web');
    expect(html).not.toContain('web · /srv/web · as');
  });

  // No shares, a share list that cannot be read, and no Taildrive leave the card body empty
  // (principle 7): the Details fold has a quiet fact, and the station's reason (a tool error)
  // is only its tooltip. This replaces the body line "Taildrive is not available: <reason>."
  test('no shares, unknown, unavailable: no body part, a Details fact', () => {
    const none = { state: 'ok' as const, shares: [], checked_at: 1, inventory_revision: 1 };
    const unknown = {
      state: 'unknown' as const,
      reason: 'Fleet Deck cannot read the share list',
      shares: [],
      checked_at: 1,
      inventory_revision: 1,
    };
    const off = {
      state: 'unavailable' as const,
      reason: 'tailscale is not installed',
      shares: [],
      checked_at: 1,
      inventory_revision: 1,
    };
    for (const td of [none, unknown, off, undefined]) expect(drive(td)).toBe('');
    expect(taildriveFact(none)).toEqual({ text: 'No shares' });
    expect(taildriveFact(unknown)).toEqual({
      text: 'Share list not readable',
      reason: 'Fleet Deck cannot read the share list',
    });
    expect(taildriveFact(off)).toEqual({
      text: 'Not available',
      reason: 'tailscale is not installed',
    });
    expect(taildriveFact(undefined)).toBeNull();
    // A station with shares lists them in the body: no fact.
    expect(taildriveFact({ ...none, shares: [share()] })).toBeNull();
    expect(renderToStaticMarkup(<TaildriveFact taildrive={off} />)).toBe(
      '<dt>Taildrive</dt><dd class="fd-facts-text" title="tailscale is not installed">Not available</dd>',
    );
    expect(renderToStaticMarkup(<TaildriveFact taildrive={none} />)).toBe(
      '<dt>Taildrive</dt><dd class="fd-facts-text">No shares</dd>',
    );
    expect(renderToStaticMarkup(<TaildriveFact taildrive={undefined} />)).toBe('');
  });

  test('an exposed share has the warning of O37 with its labels', () => {
    expect(
      exposedText(share({ exposed: ['ssh', 'project_home'], exposure_state: 'exposed' })),
    ).toBe(
      "This Taildrive share exposes ~/.ssh, a project home. Devices in your tailnet policy can read it, outside Fleet Deck's file rules.",
    );
    expect(exposedText(share({ exposed: ['member_deny'], exposure_state: 'exposed' }))).toContain(
      'a member-hidden path',
    );
    expect(
      exposedText(
        share({
          exposed: ['claude_home', 'codex_home', 'gnupg', 'aws', 'data_dir', 'worktree'],
          exposure_state: 'exposed',
        }),
      ),
    ).toContain(
      'the Claude home, the Codex home, ~/.gnupg, ~/.aws, the Fleet Deck data folder, a session worktree',
    );
    expect(exposedText(share({ exposure_state: 'exposed' }))).toContain('an unknown path');
    expect(exposedText(share())).toBeNull();
  });

  test('a missing path, an ambiguous path and an unchecked share have their notes', () => {
    expect(shareNotes(share({ path_state: 'missing' })).map((n) => n.text)).toEqual([
      'out: the path does not exist on this station.',
    ]);
    expect(shareNotes(share({ path_state: 'ambiguous' })).map((n) => n.text)).toEqual([
      'out: Fleet Deck cannot tell the exact path (it may end with spaces). Add it to [taildrive_mounts] to use it for artifacts.',
    ]);
    expect(
      shareNotes(share({ exposure_state: 'unknown', unknown_reason: 'more than 20 000 entries' })),
    ).toEqual([
      {
        text: 'out: Fleet Deck could not check all files of this share (more than 20 000 entries). It cannot be an artifact share.',
        warn: true,
      },
    ]);
    expect(shareNotes(share())).toEqual([]);
  });
});

describe('the Stations page', () => {
  const page = (stations = [station('st-a')], canAdmin = true) =>
    renderToStaticMarkup(
      <IntegrationsProvider actions={fakeActions({ canAdmin })}>
        <StationsPanel
          stations={stations}
          sessions={[]}
          freshness="live"
          actions={{ canAdmin } as never}
        />
      </IntegrationsProvider>,
    );

  test('a station card shows the traits and the Taildrive section when they are reported', () => {
    const html = page([
      station('st-a', {
        info: {
          name: 'Alpha',
          providers: [],
          traits: { os: 'linux', arch: 'x86_64', on_battery: true, labels: ['gpu'] },
          taildrive: {
            state: 'ok',
            shares: [share()],
            checked_at: 1,
            inventory_revision: 1,
          },
        } as unknown as StationInfo,
      }),
    ]);
    expect(html).toContain('Linux · x86_64 · on battery · GPU');
    expect(html).toContain('out · /srv/out · as ada');
  });

  test('an older station and an older deck show none of the new parts', () => {
    const html = page([station('st-a', { info: { name: 'Alpha', providers: [] } as never })]);
    expect(html).not.toContain('Taildrive');
    expect(html).not.toContain('Config');
    expect(html).not.toContain('linux');
  });
});
