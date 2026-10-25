import { describe, expect, test } from 'bun:test';
import { card } from '../testing/fleet';
import type { Route } from './routes';
import { buildPaneProps } from './SessionRoute';
import type { Layout } from './viewport';

type SessionRouteValue = Extract<Route, { name: 'session' }>;

const route = (tab: SessionRouteValue['tab'] = 'chat', agent?: string): SessionRouteValue => ({
  name: 'session',
  station: 'station-a',
  session: 's1',
  tab,
  ...(agent === undefined ? {} : { agent }),
});

function build(
  layout: Layout,
  tab: SessionRouteValue['tab'] = 'chat',
  opened?: () => void,
  agent?: string,
) {
  const calls: { route: Route; opts?: unknown }[] = [];
  const props = buildPaneProps({
    session: card('s1'),
    stations: [],
    timeline: undefined,
    freshness: 'live',
    actions: undefined,
    shells: undefined,
    terminals: undefined,
    watch: undefined,
    layout,
    route: route(tab, agent),
    navigate: (next, opts) => calls.push({ route: next, opts }),
    onTerminalOpened: opened,
  });
  return { props, calls };
}

describe('buildPaneProps', () => {
  test('the tab comes from the route', () => {
    expect(build('desktop', 'files').props.tab).toBe('files');
    expect(build('phone').props.tab).toBe('chat');
  });

  test('onBack exists on phone and tablet only, and goes home', () => {
    expect(build('desktop').props.onBack).toBeUndefined();
    for (const layout of ['tablet', 'phone'] as const) {
      const { props, calls } = build(layout);
      props.onBack?.();
      expect(calls).toEqual([{ route: { name: 'home' } }]);
    }
  });

  test('onTab replaces the route with the same session and the new tab', () => {
    const { props, calls } = build('desktop');
    props.onTab('changes');
    expect(calls).toEqual([{ route: route('changes'), opts: { replace: true } }]);
  });

  test('onTerminalOpened passes through only when given', () => {
    const opened = () => {};
    expect(build('phone', 'chat', opened).props.onTerminalOpened).toBe(opened);
    expect('onTerminalOpened' in build('phone').props).toBe(false);
  });

  test('layout and data props pass through', () => {
    const { props } = build('tablet');
    expect(props.layout).toBe('tablet');
    expect(props.session.summary.session_id).toBe('s1');
    expect(props.freshness).toBe('live');
  });

  test('the agent comes from the route; onAgent replaces the route, onTab drops the agent (B6)', () => {
    const none = build('desktop');
    expect('agent' in none.props).toBe(false);
    none.props.onAgent?.('node-2');
    expect(none.calls).toEqual([{ route: route('chat', 'node-2'), opts: { replace: true } }]);

    const chosen = build('desktop', 'chat', undefined, 'node-2');
    expect(chosen.props.agent).toBe('node-2');
    chosen.props.onAgent?.(null);
    chosen.props.onTab('changes');
    expect(chosen.calls).toEqual([
      { route: route('chat'), opts: { replace: true } },
      { route: route('changes'), opts: { replace: true } },
    ]);
  });
});
