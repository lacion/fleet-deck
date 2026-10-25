import { beforeEach, describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FakeTime } from '../desktop/faketime.test';
import { DeckError, deckHttp } from '../settings/http';
import { ArtifactViewerView } from './ArtifactViewer';
import {
  CONFIRM_TEXT,
  DELETED_TEXT,
  DeletionFlow,
  type DeletionPhase,
  type DeletionView,
  type DeleteActions,
  type DeleteState,
  deleteActions,
  artifactKey,
  hideArtifact,
  PENDING_TEXT,
  POLL_MS,
  POLL_WINDOW_MS,
  resetHidden,
  stateText,
  UNSUPPORTED_TEXT,
  useHiddenArtifacts,
} from './DeleteArtifact';
import { detail } from './fixtures';
import type { ViewerData } from './viewer';

const ready = (): ViewerData => {
  const d = detail('a1');
  return {
    kind: 'ok',
    value: {
      detail: d,
      version: d.artifact.latest_version,
      link: { kind: 'ok', value: '/a/t/' },
      text: null,
    },
  };
};

const view = (phase: DeletionPhase): DeletionView => ({
  phase,
  onBegin: () => {},
  onConfirm: () => {},
  onCancel: () => {},
});

const render = (deletion: DeletionView | undefined) =>
  renderToStaticMarkup(
    <ArtifactViewerView
      data={ready()}
      variant="panel"
      canShare={true}
      onVersion={() => {}}
      onBack={() => {}}
      onShare={() => {}}
      deletion={deletion}
    />,
  );

beforeEach(() => resetHidden());

describe('Delete artifact in the viewer', () => {
  test('no control without the owner actions', () => {
    expect(render(undefined)).not.toContain('Delete artifact');
  });

  test('the button, then the confirmation text, then the pending text instead of the body', () => {
    const idle = render(view({ kind: 'idle' }));
    // The last row of the More actions menu, in the danger style.
    expect(idle).toContain('aria-label="More actions"');
    expect(idle).toMatch(
      /<button type="button" class="fd-danger"><svg[\s\S]*?<\/svg> Delete artifact<\/button><\/div><\/details>/,
    );
    const confirm = render(view({ kind: 'confirm' }));
    expect(CONFIRM_TEXT).toBe(
      'Delete this artifact and all its versions? Downloaded copies and provider conversations will remain.',
    );
    expect(confirm).toContain(CONFIRM_TEXT);
    expect(confirm).not.toContain('Delete artifact');
    const hidden = render(view({ kind: 'hidden', state: { state: 'pending', reason: 'offline' } }));
    expect(hidden).toContain('Hidden on this deck. Waiting for station deletion.');
    expect(hidden).not.toContain('<iframe');
    expect(hidden).toContain('Back to the list');
    expect(render(view({ kind: 'error', text: 'Nope.' }))).toContain('Nope.');
  });

  test('the texts of each state', () => {
    expect(stateText({ state: 'pending', reason: 'offline' })).toBe(PENDING_TEXT);
    expect(stateText({ state: 'pending', reason: 'retry' })).toBe(
      'Hidden on this deck. Waiting for station deletion.',
    );
    expect(stateText({ state: 'pending', reason: 'unsupported' })).toBe(UNSUPPORTED_TEXT);
    expect(UNSUPPORTED_TEXT).toBe('Hidden on this deck. Upgrade the station to delete its copy.');
    expect(stateText({ state: 'deleted' })).toBe(DELETED_TEXT);
    expect(DELETED_TEXT).toBe('Deleted.');
  });
});

describe('the deletion calls', () => {
  test('DELETE carries the elevation header; the deletion route is read with GET', async () => {
    const seen: { url: string; method: string; headers: Record<string, string> }[] = [];
    const io = deckHttp(
      async (url, init) => {
        seen.push({
          url: String(url),
          method: String(init?.method),
          headers: init?.headers as Record<string, string>,
        });
        return new Response(JSON.stringify({ state: 'pending', reason: 'retry' }), {
          status: 202,
          headers: { 'Content-Type': 'application/json' },
        });
      },
      () => 'csrf',
      1000,
    );
    const actions = deleteActions(io);
    expect(await actions.remove('st 1', 'a/1', 'ELEV')).toEqual({
      state: 'pending',
      reason: 'retry',
    });
    expect(seen[0]?.url).toBe('/api/artifacts/st%201/a%2F1');
    expect(seen[0]?.method).toBe('DELETE');
    expect(seen[0]?.headers['X-FleetDeck-Elevation']).toBe('ELEV');
    await actions.state('st 1', 'a/1');
    expect(seen[1]?.url).toBe('/api/artifacts/st%201/a%2F1/deletion');
    expect(seen[1]?.method).toBe('GET');
  });

  test('an invalid answer is refused', async () => {
    const io = deckHttp(
      async () => new Response('{"state":"weird"}'),
      () => null,
      1000,
    );
    await expect(deleteActions(io).state('s', 'a')).rejects.toThrow('invalid');
  });
});

function flowSetup(over: {
  ask?: () => Promise<string | null>;
  remove?: () => Promise<DeleteState>;
  state?: () => Promise<DeleteState>;
}) {
  const time = new FakeTime();
  const calls = { ask: 0, remove: 0, state: 0, leave: 0 };
  const hidden: string[] = [];
  const actions: DeleteActions = {
    remove: async () => {
      calls.remove += 1;
      return over.remove ? over.remove() : { state: 'deleted' };
    },
    state: async () => {
      calls.state += 1;
      return over.state ? over.state() : { state: 'pending', reason: 'retry' };
    },
  };
  const flow = new DeletionFlow({
    station: 'st',
    id: 'a1',
    actions,
    ask: async () => {
      calls.ask += 1;
      return over.ask ? over.ask() : 'ELEV';
    },
    leave: () => {
      calls.leave += 1;
    },
    hide: (s, i) => {
      hidden.push(`${s}/${i}`);
    },
    now: time.clock,
    timers: time.timers,
  });
  return { flow, time, calls, hidden };
}

describe('the deletion flow', () => {
  test('begin opens the confirmation; cancel closes it and sends nothing', () => {
    const { flow, calls } = flowSetup({});
    flow.begin();
    expect(flow.getSnapshot().kind).toBe('confirm');
    flow.cancel();
    expect(flow.getSnapshot().kind).toBe('idle');
    expect(calls.remove).toBe(0);
  });

  test('a cancelled elevation changes nothing and sends nothing', async () => {
    const { flow, calls, hidden } = flowSetup({ ask: async () => null });
    flow.begin();
    await flow.confirm();
    expect(calls.ask).toBe(1);
    expect(calls.remove).toBe(0);
    expect(hidden).toEqual([]);
    expect(calls.leave).toBe(0);
    expect(flow.getSnapshot().kind).toBe('idle');
  });

  test('200 hides the artifact and leaves the viewer', async () => {
    const { flow, calls, hidden } = flowSetup({});
    flow.begin();
    await flow.confirm();
    expect(calls.remove).toBe(1);
    expect(hidden).toEqual(['st/a1']);
    expect(calls.leave).toBe(1);
  });

  test('202 with each reason hides the artifact and shows its text', async () => {
    for (const [reason, text] of [
      ['offline', PENDING_TEXT],
      ['retry', PENDING_TEXT],
      ['unsupported', UNSUPPORTED_TEXT],
    ] as const) {
      const { flow, calls, hidden } = flowSetup({
        remove: async () => ({ state: 'pending', reason }),
      });
      flow.begin();
      await flow.confirm();
      const phase = flow.getSnapshot();
      expect(phase.kind).toBe('hidden');
      expect(phase.kind === 'hidden' ? stateText(phase.state) : '').toBe(text);
      expect(hidden).toEqual(['st/a1']);
      expect(calls.leave).toBe(0);
    }
  });

  test('202 polls every 5 s until the deck says deleted: "Deleted."', async () => {
    let polls = 0;
    const { flow, time, calls } = flowSetup({
      remove: async () => ({ state: 'pending', reason: 'offline' }),
      state: async () => {
        polls += 1;
        return polls < 3 ? { state: 'pending', reason: 'offline' } : { state: 'deleted' };
      },
    });
    flow.begin();
    await flow.confirm();
    await time.advance(POLL_MS);
    expect(calls.state).toBe(1);
    await time.advance(POLL_MS * 5);
    expect(calls.state).toBe(3);
    const phase = flow.getSnapshot();
    expect(phase.kind === 'hidden' ? stateText(phase.state) : '').toBe('Deleted.');
    expect(time.pending).toBe(0);
  });

  test('the polling ends after 60 s of pending and after dispose', async () => {
    const { flow, time, calls } = flowSetup({
      remove: async () => ({ state: 'pending', reason: 'retry' }),
    });
    flow.begin();
    await flow.confirm();
    await time.advance(POLL_WINDOW_MS + POLL_MS * 5);
    expect(calls.state).toBe(POLL_WINDOW_MS / POLL_MS);
    expect(time.pending).toBe(0);

    const second = flowSetup({ remove: async () => ({ state: 'pending', reason: 'retry' }) });
    second.flow.begin();
    await second.flow.confirm();
    second.flow.dispose();
    await second.time.advance(POLL_MS * 3);
    expect(second.calls.state).toBe(0);
  });

  test('an error shows the deck message and allows another try', async () => {
    const { flow, hidden } = flowSetup({
      remove: async () => {
        throw new DeckError(400, 'a recent sign-in is needed', 'elevation_required', {});
      },
    });
    flow.begin();
    await flow.confirm();
    expect(flow.getSnapshot()).toEqual({ kind: 'error', text: 'a recent sign-in is needed' });
    expect(hidden).toEqual([]);
    flow.begin();
    expect(flow.getSnapshot().kind).toBe('confirm');
  });
});

describe('hidden artifacts', () => {
  const Probe = () => <p>{[...useHiddenArtifacts()].join(',')}</p>;

  test('the set is empty, then holds the hidden artifact, then is reset', () => {
    expect(renderToStaticMarkup(<Probe />)).toBe('<p></p>');
    hideArtifact('st', 'a1');
    expect(renderToStaticMarkup(<Probe />)).toBe('<p>st/a1</p>');
    expect(artifactKey('st', 'a1')).toBe('st/a1');
    resetHidden();
    expect(renderToStaticMarkup(<Probe />)).toBe('<p></p>');
  });
});
