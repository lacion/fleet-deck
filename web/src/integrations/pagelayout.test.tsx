// The UI audit (round 1): the Projects list and the project page header follow the design system.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { station } from '../testing/fleet';
import { IntegrationsProvider } from './context';
import { fakeActions, NOW, project } from './fixtures';
import { ProjectPage } from './ProjectPage';
import { ProjectsPage } from './ProjectsPage';

const noop = () => {};

const provide = (node: React.ReactNode, projects = [project()]) =>
  renderToStaticMarkup(
    <IntegrationsProvider actions={fakeActions()} initial={projects}>
      {node}
    </IntegrationsProvider>,
  );

describe('the projects list', () => {
  test('the description says where the tokens are; no note floats under the table', () => {
    const html = provide(<ProjectsPage stations={[station()]} />);
    expect(html).toContain(
      '<p class="fd-page-desc">Repositories with their forge and trackers. Forge tokens stay on the station.</p>',
    );
    expect(html).not.toContain('bindings');
    expect(provide(<ProjectsPage stations={[station()]} />, [])).not.toContain('bindings');
  });

  test('each row has one actions menu; the table sits on the canvas; Refresh is an icon', () => {
    const html = provide(<ProjectsPage stations={[station()]} />);
    expect(html).toContain('aria-label="Actions for Atlas"');
    expect(html).toMatch(
      /<button type="button" class="fd-danger"><svg[\s\S]*?<\/svg> Delete…<\/button>/,
    );
    expect(html).not.toContain('fd-tablepanel');
    expect(html).toMatch(/<button type="button" class="fd-iconbtn" aria-label="Refresh"/);
  });

  test('an empty list holds the one primary in its empty state, not in the header', () => {
    const html = provide(<ProjectsPage stations={[station()]} />, []);
    expect(html.match(/New project/g)).toHaveLength(1);
    expect(html.indexOf('New project')).toBeGreaterThan(html.indexOf('fd-emptystate'));
  });
});

describe('the project page header', () => {
  test('it says where the project is and has its chips; the setup is a tab of its own', () => {
    const html = provide(
      <ProjectPage id="p1" stations={[station()]} sessions={[]} onFixChecks={noop} now={NOW} />,
    );
    expect(html).toContain(
      '<p class="fd-page-desc"><span class="fd-int-where"><span>Alpha</span><span class="fd-int-sep" aria-hidden="true">·</span><code class="fd-int-path" title="/work/atlas"><bdi>/work/atlas</bdi></code></span></p>',
    );
    expect(html).toMatch(/<div class="fd-page-filters"><span class="fd-int-chips">/);
    // The sessions are first under the header: no setup panel before the tabs.
    expect(html.indexOf('role="tablist"')).toBeLessThan(html.indexOf('fd-int-tab'));
    expect(html).toContain('>Setup<');
    expect(html).not.toContain('aria-label="Homes"');
    const setup = provide(
      <ProjectPage
        id="p1"
        stations={[station()]}
        sessions={[]}
        onFixChecks={noop}
        now={NOW}
        initialTab="setup"
      />,
    );
    expect(setup).toContain('class="fd-int-tab fd-int-setup"');
  });
});
