import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { Folder } from './icons';
import { EmptyState, Field, FormActions, FormGrid, fieldError, Page, Section } from './page';

describe('Page (DL-171)', () => {
  test('the title row, the count, the actions and the description', () => {
    const html = renderToStaticMarkup(
      <Page
        title="Projects"
        count={3}
        description="A project is a repository."
        actions={<button type="button">New project</button>}
      >
        <p>body</p>
      </Page>,
    );
    expect(html).toContain('<section class="fd-page" aria-label="Projects">');
    expect(html).toContain('<h1 class="fd-page-title">Projects</h1>');
    expect(html).toContain('<span class="fd-count">3</span>');
    expect(html).toContain(
      '<div class="fd-page-actions"><button type="button">New project</button>',
    );
    expect(html).toContain('<p class="fd-page-desc">A project is a repository.</p>');
    expect(html).toContain('<p>body</p>');
  });

  test('the filters sit in the header row before the actions (§4.3)', () => {
    const html = renderToStaticMarkup(
      <Page
        title="Watch"
        filters={<button type="button">Needs you only</button>}
        actions={<button type="button">Share</button>}
      >
        <p />
      </Page>,
    );
    const head = html.slice(html.indexOf('<header'), html.indexOf('</header>'));
    expect(head).toContain(
      '<div class="fd-page-filters"><button type="button">Needs you only</button></div>',
    );
    expect(head.indexOf('fd-page-filters')).toBeLessThan(head.indexOf('fd-page-actions'));
    const none = renderToStaticMarkup(
      <Page title="Plans">
        <p />
      </Page>,
    );
    expect(none).not.toContain('fd-page-filters');
  });

  test('a wide page, a label and an extra class', () => {
    const html = renderToStaticMarkup(
      <Page title="Plans" wide label="Plan library" className="fd-x">
        <p />
      </Page>,
    );
    expect(html).toContain('class="fd-page fd-x"');
    expect(html).toContain('data-wide="true"');
    expect(html).toContain('aria-label="Plan library"');
    expect(html).not.toContain('fd-page-actions');
    expect(html).not.toContain('fd-page-desc');
  });

  test('a section is a label row and a panel', () => {
    const html = renderToStaticMarkup(
      <Section label="Forge">
        <p>x</p>
      </Section>,
    );
    expect(html).toContain('<h2 class="fd-pagesec-title">Forge</h2>');
    expect(html).toContain('class="fd-panel fd-pagesec-body"');
  });
});

describe('EmptyState (DL-171)', () => {
  test('an icon, a title, a sentence and an action', () => {
    const html = renderToStaticMarkup(
      <EmptyState
        icon={Folder}
        title="No shares yet"
        body="Share a session."
        action={<button type="button">New share</button>}
      />,
    );
    expect(html).toContain('class="fd-panel fd-emptystate"');
    expect(html).toContain('<p class="fd-emptystate-title">No shares yet</p>');
    expect(html).toContain('<p class="fd-emptystate-body">Share a session.</p>');
    expect(html).toContain('New share</button>');
    expect(html).toContain('width="20"');
  });
});

describe('Field and the form parts (DL-171)', () => {
  test('a field has its label, the control and a hint', () => {
    const html = renderToStaticMarkup(
      <Field label="Name" hint="1 to 80 characters">
        <input />
      </Field>,
    );
    expect(html).toContain('<span class="fd-fieldlabel">Name</span>');
    expect(html).toContain('<input/>');
    expect(html).toContain('<span class="fd-fieldhint">1 to 80 characters</span>');
    expect(html).not.toContain('role="alert"');
  });

  test('an error replaces the hint', () => {
    const html = renderToStaticMarkup(
      <Field label="Name" hint="1 to 80 characters" error="The name is required.">
        <input />
      </Field>,
    );
    expect(html).toContain('<span class="fd-fielderror" role="alert">The name is required.</span>');
    expect(html).not.toContain('fd-fieldhint');
    expect(html).toContain('data-invalid="true"');
  });

  test('an error shows only after the field was touched or a submit was tried', () => {
    expect(fieldError('Required.', false, false)).toBeNull();
    expect(fieldError('Required.', true, false)).toBe('Required.');
    expect(fieldError('Required.', false, true)).toBe('Required.');
    expect(fieldError(null, true, true)).toBeNull();
  });

  test('span 2 and a group', () => {
    const html = renderToStaticMarkup(
      <Field label="Folder" span={2} group>
        <input />
      </Field>,
    );
    expect(html).toContain('data-span="2"');
    expect(html).toContain('role="group"');
    expect(html).not.toContain('<label');
  });

  test('the grid and the form actions', () => {
    expect(renderToStaticMarkup(<FormGrid>x</FormGrid>)).toContain('class="fd-formgrid"');
    expect(
      renderToStaticMarkup(
        <FormActions>
          <button type="button">Cancel</button>
        </FormActions>,
      ),
    ).toContain('class="fd-formactions"');
  });
});
