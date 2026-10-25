// The fold of a forge connect block: "Connect another … account…" opens it, a login keeps it open,
// and after a login the same button closes it again (it was stuck open with aria-expanded="true").
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { AccountActions } from './actions';
import { ForgeConnect, foldOf, type Run } from './ForgeSection';

const LOGIN = {
  login_id: 'l1',
  code: 'WXYZ-9999',
  url: 'https://github.com/login/device',
  expires_in: 900,
};
const running = (state: 'pending' | 'done'): Run => ({
  phase: 'running',
  login: LOGIN,
  state: state === 'done' ? { state: 'done', user: 'ana' } : { state: 'pending' },
});

const block = (run: Run) =>
  renderToStaticMarkup(
    <ForgeConnect
      kind="github"
      station="station-a"
      actions={{} as AccountActions}
      connected
      onConnected={() => {}}
      onToken={() => {}}
      initialRun={run}
    />,
  );

describe('the fold of a connect block', () => {
  test('without an account the block is open and has no fold button', () => {
    expect(foldOf(false, false, { phase: 'idle' })).toEqual({ shown: true, locked: false });
    expect(
      renderToStaticMarkup(
        <ForgeConnect
          kind="gitlab"
          station="station-a"
          actions={{} as AccountActions}
          onConnected={() => {}}
          onToken={() => {}}
        />,
      ),
    ).not.toContain('aria-expanded');
  });

  test('with an account it folds; a press opens it', () => {
    expect(foldOf(true, false, { phase: 'idle' })).toEqual({ shown: false, locked: false });
    expect(foldOf(true, true, { phase: 'idle' })).toEqual({ shown: true, locked: false });
  });

  test('a login that starts or waits keeps it open, and the button is off', () => {
    expect(foldOf(true, false, { phase: 'starting' })).toEqual({ shown: true, locked: true });
    expect(foldOf(true, false, running('pending'))).toEqual({ shown: true, locked: true });
    expect(block(running('pending'))).toMatch(/aria-expanded="true" disabled=""/);
  });

  test('after a login the block shows the result and the button can close it', () => {
    expect(foldOf(true, false, running('done'))).toEqual({ shown: true, locked: false });
    const html = block(running('done'));
    expect(html).toContain('Connected as ana.');
    expect(html).toMatch(/aria-expanded="true"><svg/);
    expect(html).not.toMatch(/aria-expanded="true" disabled=""/);
  });
});
