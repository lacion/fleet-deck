import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { freshRequests, waitingRequests } from '../board/notify';
import { card } from '../testing/fleet';
import { sessionActions } from './commands';
import { Composer } from './Composer';
import { fromDataUrl, imagesProblem, MAX_IMAGE_BYTES, MAX_IMAGES, readImage } from './images';

const png = { media_type: 'image/png', data: 'iVBORw0KGgo=' };

describe('image paste into prompts (DL-304)', () => {
  test('a data URL becomes an image input and other forms are refused', () => {
    expect(fromDataUrl('data:image/png;base64,iVBORw0KGgo=')).toEqual(png);
    expect(fromDataUrl('data:text/plain,hello')).toBeNull();
    expect(fromDataUrl('https://example.com/a.png')).toBeNull();
  });

  test('the board checks the deck limits before it sends', () => {
    expect(imagesProblem([png])).toBeNull();
    expect(imagesProblem(Array(MAX_IMAGES + 1).fill(png))).toContain('at most 4 images');
    expect(imagesProblem([{ media_type: 'image/svg+xml', data: 'AAAA' }])).toContain(
      'image/svg+xml',
    );
    expect(imagesProblem([{ ...png, data: 'A'.repeat(MAX_IMAGE_BYTES + 4) }])).toContain(
      'too large',
    );
  });

  test('a small image file is read as base64 without a change', async () => {
    const bytes = Uint8Array.from([0x89, 0x50, 0x4e, 0x47]);
    const read = await readImage(new Blob([bytes], { type: 'image/png' }));
    expect(read).toEqual({ media_type: 'image/png', data: 'iVBORw==' });
  });

  test('a prompt with images sends them in the input; a prompt without keeps the old form', async () => {
    const posts: unknown[] = [];
    const actions = sessionActions(
      async (_path, body) => {
        posts.push(body);
        return { state: 'done', receipt: { state: 'accepted' } };
      },
      { station_id: 'station-a', session_id: 's1' },
      ['drive'],
      () => 'c1',
    );
    await actions.send('prompt', 'what is this?', [png]);
    await actions.send('steer', 'go on');
    expect(posts).toEqual([
      {
        station: 'station-a',
        command_id: 'c1',
        command: {
          op: 'prompt',
          session_id: 's1',
          input: { text: 'what is this?', images: [png] },
        },
      },
      {
        station: 'station-a',
        command_id: 'c1',
        command: { op: 'steer', session_id: 's1', input: { text: 'go on' } },
      },
    ]);
  });

  test('the composer offers an image picker for phones', () => {
    const html = renderToStaticMarkup(
      <Composer enabled working={false} steer={false} nativeQueue onSend={() => {}} />,
    );
    expect(html).toContain('Add image');
    expect(html).toContain('accept="image/png,image/jpeg,image/gif,image/webp"');
  });
});

describe('needs-you notifications (DL-305)', () => {
  const req = (id: string) => ({
    request_id: id,
    node_id: null,
    request_kind: 'command' as const,
    title: `Run ${id}`,
    details: 'ls',
    input: null,
    options: [],
    response: 'rpc_reply' as const,
    requested_at: 1,
  });

  test('only a request that was not open before makes a notification', () => {
    const before = waitingRequests([
      card('s1', 'needs_you', 'station-a', { open_requests: [req('r1')] }),
    ]);
    const after = waitingRequests([
      card('s1', 'needs_you', 'station-a', { open_requests: [req('r1'), req('r2')] }),
      card('s2', 'needs_you', 'station-b', { open_requests: [req('r1')] }),
    ]);
    const fresh = freshRequests(new Set(before.map((w) => w.id)), after);
    expect(fresh.map((w) => w.id)).toEqual(['station-a/s1/r2', 'station-b/s2/r1']);
    expect(fresh[0]?.request).toBe('Run r2');
    expect(fresh[1]?.key).toEqual({ station_id: 'station-b', session_id: 's2' });
  });
});
