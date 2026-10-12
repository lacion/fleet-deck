import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { Diff, splitDiffFileLine } from './Diff';

const patch = [
  'diff --git a/src/a.ts b/src/a.ts',
  'index 83db48f..bf2a1c3 100644',
  '--- a/src/a.ts',
  '+++ b/src/a.ts',
  '@@ -1 +1 @@',
  '-old',
  '+new',
].join('\n');

describe('Diff', () => {
  test('a file line is plain text without onOpenFile', () => {
    const html = renderToStaticMarkup(<Diff patch={patch} truncated={false} id="t1" />);
    expect(html).toContain('diff --git a/src/a.ts b/src/a.ts');
    expect(html).not.toContain('fd-dl-file');
  });

  test('a file line has the path as a button with onOpenFile', () => {
    const html = renderToStaticMarkup(
      <Diff patch={patch} truncated={false} id="t1" onOpenFile={() => {}} />,
    );
    expect(html).toContain(
      'diff --git a/src/a.ts b/<button type="button" class="fd-dl-file">src/a.ts</button>',
    );
    // only the file line has a button
    expect(html.match(/fd-dl-file/g)).toHaveLength(1);
  });

  test('splitDiffFileLine takes the path after b/', () => {
    expect(splitDiffFileLine('diff --git a/x/old.ts b/x/new.ts')).toEqual({
      head: 'diff --git a/x/old.ts b/',
      path: 'x/new.ts',
    });
    expect(splitDiffFileLine('+diff --git a/x b/x')).toBeNull();
    expect(splitDiffFileLine('index 1..2')).toBeNull();
  });
});
