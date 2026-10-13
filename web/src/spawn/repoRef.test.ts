import { describe, expect, test } from 'bun:test';
import { type CloneDefaults, cloneDefaultsOf, DEFAULT_CLONE, resolveRepo } from './repoRef';

const ssh: CloneDefaults = { host: 'gitlab.com', org: '', transport: 'ssh' };
const acme: CloneDefaults = { host: 'github.com', org: 'acme', transport: 'https' };
const group: CloneDefaults = { host: 'gitlab.com', org: 'acme/tools', transport: 'https' };

describe('resolveRepo', () => {
  test('owner/name uses the default host and transport', () => {
    expect(resolveRepo('acme/app', DEFAULT_CLONE)).toEqual({
      url: 'https://github.com/acme/app.git',
      name: 'app',
    });
    expect(resolveRepo('acme/app', ssh)).toEqual({
      url: 'git@gitlab.com:acme/app.git',
      name: 'app',
    });
  });

  test('host/owner/name names its host and lower-cases it', () => {
    expect(resolveRepo('gitlab.com/acme/app', DEFAULT_CLONE)).toEqual({
      url: 'https://gitlab.com/acme/app.git',
      name: 'app',
    });
    expect(resolveRepo('GitLab.com/acme/app', ssh)).toEqual({
      url: 'git@gitlab.com:acme/app.git',
      name: 'app',
    });
  });

  test('a GitLab subgroup stays in the path', () => {
    expect(resolveRepo('gitlab.com/g/sub/name', DEFAULT_CLONE)).toEqual({
      url: 'https://gitlab.com/g/sub/name.git',
      name: 'name',
    });
    expect(resolveRepo('g/sub/name', ssh)).toEqual({
      url: 'git@gitlab.com:g/sub/name.git',
      name: 'name',
    });
  });

  test('a bare name needs the default organization', () => {
    expect(resolveRepo('app', DEFAULT_CLONE)).toEqual({
      error: 'Set a default organization in Settings, or enter owner/name.',
    });
    expect(resolveRepo('app', acme)).toEqual({
      url: 'https://github.com/acme/app.git',
      name: 'app',
    });
    expect(resolveRepo('app', group)).toEqual({
      url: 'https://gitlab.com/acme/tools/app.git',
      name: 'app',
    });
  });

  test('a .git suffix and slashes around the path are dropped', () => {
    expect(resolveRepo('  acme/app.git ', DEFAULT_CLONE)).toEqual({
      url: 'https://github.com/acme/app.git',
      name: 'app',
    });
    expect(resolveRepo('/acme/app/', DEFAULT_CLONE)).toEqual({
      url: 'https://github.com/acme/app.git',
      name: 'app',
    });
    expect(resolveRepo('app.git', acme)).toEqual({
      url: 'https://github.com/acme/app.git',
      name: 'app',
    });
  });

  test('a URL is kept as it is and gives its repository name', () => {
    expect(resolveRepo('https://github.com/acme/app.git', ssh)).toEqual({
      url: 'https://github.com/acme/app.git',
      name: 'app',
    });
    expect(resolveRepo('https://github.com/acme/app', ssh)).toEqual({
      url: 'https://github.com/acme/app',
      name: 'app',
    });
    expect(resolveRepo('ssh://git@host.example:2222/g/sub/tool.git/', ssh)).toEqual({
      url: 'ssh://git@host.example:2222/g/sub/tool.git/',
      name: 'tool',
    });
    expect(resolveRepo('git@github.com:acme/app.git', DEFAULT_CLONE)).toEqual({
      url: 'git@github.com:acme/app.git',
      name: 'app',
    });
    expect(resolveRepo('git@host.example:app.git', DEFAULT_CLONE)).toEqual({
      url: 'git@host.example:app.git',
      name: 'app',
    });
  });

  test('a URL without a repository name is refused', () => {
    for (const url of ['https://github.com', 'https://github.com/', 'https://github.com/.git']) {
      expect(resolveRepo(url, DEFAULT_CLONE)).toEqual({ error: 'The URL has no repository name.' });
    }
  });

  test('an empty input is refused', () => {
    expect(resolveRepo('   ', DEFAULT_CLONE)).toEqual({ error: 'Enter a repository.' });
  });

  test('bad segments and dot segments are refused', () => {
    const bad = { error: 'Use owner/name, host/owner/name or a URL.' };
    for (const input of [
      'acme/a b',
      'acme//app',
      'a/../b',
      '../app',
      './app',
      'acme/app?x',
      '..',
      'a:b/c',
    ]) {
      expect(resolveRepo(input, acme)).toEqual(bad);
    }
  });
});

describe('cloneDefaultsOf', () => {
  test('reads the three deck values', () => {
    expect(
      cloneDefaultsOf({
        values: { clone_host: 'gitlab.com', clone_org: 'acme/tools', clone_transport: 'ssh' },
      }),
    ).toEqual({ host: 'gitlab.com', org: 'acme/tools', transport: 'ssh' });
  });

  test('a missing or odd value gives the built-in one', () => {
    expect(cloneDefaultsOf({ values: {} })).toEqual(DEFAULT_CLONE);
    expect(
      cloneDefaultsOf({ values: { clone_host: '', clone_org: 3, clone_transport: 'ftp' } }),
    ).toEqual(DEFAULT_CLONE);
  });
});
