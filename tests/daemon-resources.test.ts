import assert from 'node:assert/strict';
import { describe, test } from 'bun:test';
import {
  DaemonResources,
  type CloseOwner,
  type CoreLifecycleOwner,
  type HttpLifecycleOwner,
} from '../src/daemon/daemon-resources.ts';

function owner(events: string[], name: string, fail = false): CloseOwner {
  return {
    async close() {
      events.push(name);
      if (fail) throw new Error(`${name} failed`);
    },
  };
}

function coreOwner(events: string[]): CoreLifecycleOwner {
  return {
    quiesce() {
      events.push('core.quiesce');
    },
    close() {
      events.push('core.close');
    },
  };
}

function httpOwner(events: string[]): HttpLifecycleOwner {
  return {
    quiesce() {
      events.push('http.quiesce');
    },
    releaseHolds() {
      events.push('http.releaseHolds');
    },
    close() {
      events.push('http.close');
    },
  };
}

describe('DaemonResources', () => {
  test('every acquisition prefix closes only what exists in policy order', async () => {
    const expected = [
      [],
      ['pid'],
      ['db', 'pid'],
      ['core.quiesce', 'core.close', 'db', 'pid'],
      [
        'http.quiesce',
        'core.quiesce',
        'http.releaseHolds',
        'http.close',
        'core.close',
        'db',
        'pid',
      ],
      [
        'http.quiesce',
        'core.quiesce',
        'agents',
        'http.releaseHolds',
        'http.close',
        'core.close',
        'db',
        'pid',
      ],
      [
        'http.quiesce',
        'core.quiesce',
        'agents',
        'mdns',
        'http.releaseHolds',
        'http.close',
        'core.close',
        'db',
        'pid',
      ],
    ];

    for (let prefix = 0; prefix <= 6; prefix += 1) {
      const events: string[] = [];
      const resources = new DaemonResources();
      if (prefix >= 1) resources.setProcess('pid', owner(events, 'pid'));
      if (prefix >= 2) resources.setStore('db', owner(events, 'db'));
      if (prefix >= 3) resources.setCore(coreOwner(events));
      if (prefix >= 4) resources.setHttp(httpOwner(events));
      if (prefix >= 5) resources.addProducer('agents', owner(events, 'agents'));
      if (prefix >= 6) resources.addDiscovery('mdns', owner(events, 'mdns'));

      await resources.close();
      assert.deepEqual(events, expected[prefix], `acquisition prefix ${prefix}`);
    }
  });

  test('concurrent and repeated close share one completion and run each phase once', async () => {
    const events: string[] = [];
    const resources = new DaemonResources({
      http: httpOwner(events),
      core: coreOwner(events),
      producers: [
        { name: 'first', owner: owner(events, 'first') },
        { name: 'second', owner: owner(events, 'second') },
      ],
      discovery: [{ name: 'mdns', owner: owner(events, 'mdns') }],
      store: { name: 'db', owner: owner(events, 'db') },
      process: { name: 'pid', owner: owner(events, 'pid') },
    });

    const first = resources.close();
    const second = resources.close();
    assert.equal(first, second);
    await Promise.all([first, second, resources.close()]);

    assert.deepEqual(events, [
      'http.quiesce',
      'core.quiesce',
      'second',
      'first',
      'mdns',
      'http.releaseHolds',
      'http.close',
      'core.close',
      'db',
      'pid',
    ]);
  });

  test('publishes the close promise before an owner callback can re-enter', async () => {
    const events: string[] = [];
    let resources: DaemonResources;
    let reentered: Promise<void> | null = null;
    resources = new DaemonResources({
      http: {
        quiesce() {
          events.push('http.quiesce');
          reentered = resources.close();
        },
        releaseHolds() {
          events.push('http.releaseHolds');
        },
        close() {
          events.push('http.close');
        },
      },
    });

    const closing = resources.close();
    await closing;

    assert.equal(reentered, closing);
    assert.deepEqual(events, ['http.quiesce', 'http.releaseHolds', 'http.close']);
  });

  test('one failing DB user skips store retirement but still releases process ownership', async () => {
    const events: string[] = [];
    const diagnostics: string[] = [];
    const resources = new DaemonResources({
      producers: [{ name: 'bad-producer', owner: owner(events, 'bad-producer', true) }],
      store: { name: 'db', owner: owner(events, 'db') },
      process: { name: 'pid', owner: owner(events, 'pid') },
      onCloseError(name) {
        diagnostics.push(name);
      },
    });

    await resources.close();

    assert.deepEqual(events, ['bad-producer', 'pid']);
    assert.deepEqual(diagnostics, ['bad-producer']);
    assert.equal(resources.closeErrors.length, 1);
    assert.throws(() => resources.addProducer('late', owner(events, 'late')), /closing/);
  });
});
