import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { type ClientState, defaultClientState, parseClientState } from '../../src/model/clientState';
import type { DecimalU64, HostId } from '../../src/model/ids';
import type { LayoutSnapshot } from '../../src/model/layout';
import { App } from '../../src/shell/App';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryLayoutPort } from '../support/memoryLayoutPort';
import { MemoryTerminalPort } from '../support/memoryTerminalPort';
import { HOST_A, host, snapshot, space } from '../support/snapshots';

const connected = { kind: 'connected' } as const;
const PATH = '/src/alpha';

function hosts() {
  return snapshot([host('mac', HOST_A, connected, [space(HOST_A, PATH), space(HOST_A, '/src/beta')])]);
}

function layout(readOnly = false): LayoutSnapshot {
  return {
    space: PATH,
    layoutRevision: '3' as DecimalU64,
    root: {
      type: 'split',
      dir: 'h',
      ratio: 0.5,
      a: { type: 'tile', id: 't1', panels: ['p1', 'p2'], active: 'p1' },
      b: { type: 'tile', id: 't2', panels: ['p3'], active: null },
    },
    readOnly,
    error: null,
    dropped: null,
  };
}

function saved(focusedTile: string | null, activeTabs: Record<string, string>): ClientState {
  return {
    ...defaultClientState(),
    lastSpace: { spaceId: PATH, selectedAtMs: '5' as DecimalU64 },
    spaces: {
      [PATH]: { spaceId: PATH, focusedTile, activeTerminal: null, activeTabs, groupState: {} },
    },
  };
}

async function mount(state: ClientState | null) {
  const layoutPort = new MemoryLayoutPort();
  const clientStatePort = new MemoryClientStatePort(
    state === null ? new Map() : new Map<HostId, ClientState>([[HOST_A, state]]),
  );
  render(
    <App
      terminalPort={new MemoryTerminalPort()}
      port={new MemoryHostsPort(hosts())}
      clientStatePort={clientStatePort}
      layoutPort={layoutPort}
      newTileId={() => 't-new'}
    />,
  );
  return { layoutPort, clientStatePort };
}

const focused = () =>
  [...document.querySelectorAll('[data-tile-id][aria-current="true"]')].map((node) =>
    node.getAttribute('data-tile-id'),
  );
const selectedTab = (tile: string) =>
  document
    .querySelector(`[data-tile-id="${tile}"] [role="tab"][aria-selected="true"]`)
    ?.getAttribute('data-panel-id');

describe('App with a layout', () => {
  it('read-only snapshot marks the rail', async () => {
    const { layoutPort } = await mount(saved(null, {}));
    await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
    await waitFor(() => expect(layoutPort.listenerCount()).toBe(1));
    const rail = () => screen.getByRole('button', { name: /^alpha/ });
    expect(rail().getAttribute('data-read-only')).toBeNull();
    act(() => layoutPort.push(HOST_A, layout(true)));
    expect(rail().getAttribute('data-read-only')).toBe('true');
    act(() => layoutPort.push(HOST_A, { ...layout(false), layoutRevision: '4' as DecimalU64 }));
    expect(rail().getAttribute('data-read-only')).toBeNull();
  });

  it('restores focused tile and active tabs from client state', async () => {
    const { layoutPort } = await mount(saved('t2', { t1: 'p2' }));
    await waitFor(() => expect(layoutPort.listenerCount()).toBe(1));
    await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
    expect(layoutPort.calls[0]).toEqual({ kind: 'watch', hostId: HOST_A, space: PATH });
    act(() => layoutPort.push(HOST_A, layout()));
    await waitFor(() => expect(focused()).toEqual(['t2']));
    expect(selectedTab('t1')).toBe('p2');
    expect(selectedTab('t2')).toBe('p3');
  });

  it('falls back when focused tile vanished', async () => {
    const { layoutPort } = await mount(saved('gone', { t1: 'p9', gone: 'p1' }));
    await waitFor(() => expect(layoutPort.listenerCount()).toBe(1));
    await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
    act(() => layoutPort.push(HOST_A, layout()));
    await waitFor(() => expect(focused()).toEqual(['t1']));
    expect(selectedTab('t1')).toBe('p1');
  });

  it('persists the focused tile and active tab and sends a split', async () => {
    const { layoutPort, clientStatePort } = await mount(saved(null, {}));
    await waitFor(() => expect(layoutPort.listenerCount()).toBe(1));
    await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
    act(() => layoutPort.push(HOST_A, layout()));
    const tile = await waitFor(() => {
      const found = document.querySelector('[data-tile-id="t2"]');
      if (found === null) throw new Error('no tile yet');
      return found;
    });
    fireEvent.click(tile.querySelector('[role="tab"]') as HTMLElement);
    await waitFor(() => expect(focused()).toEqual(['t2']));
    const writes = clientStatePort.writes.filter((write) => write.kind === 'space');
    expect(writes.at(-1)).toMatchObject({
      path: PATH,
      state: { focusedTile: 't2', activeTabs: { t2: 'p3' } },
    });
    fireEvent.click(tile.querySelector('button:not([role="tab"])') as HTMLElement);
    await waitFor(() => expect(layoutPort.calls.at(-1)?.kind).toBe('mutate'));
  });

  it('Split down and Close tile send the exact ops', async () => {
    const { layoutPort } = await mount(saved(null, {}));
    await waitFor(() => expect(layoutPort.listenerCount()).toBe(1));
    await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
    act(() => layoutPort.push(HOST_A, layout()));
    const t2 = await waitFor(() => {
      const found = document.querySelector('[data-tile-id="t2"]');
      if (found === null) throw new Error('no tile yet');
      return found as HTMLElement;
    });
    const mutates = () => layoutPort.calls.filter((call) => call.kind === 'mutate');
    fireEvent.click(within(t2).getByRole('button', { name: 'Split down' }));
    await waitFor(() => expect(mutates()).toHaveLength(1));
    expect(mutates()[0]).toMatchObject({
      baseLayoutRevision: '3',
      ops: [{ op: 'split_tile', tile: 't2', dir: 'v', newTile: 't-new', ratio: 0.5, panel: null }],
    });
    act(() => layoutPort.push(HOST_A, { ...layout(), layoutRevision: '4' as DecimalU64 }, '2' as DecimalU64));
    fireEvent.click(within(t2).getByRole('button', { name: 'Close tile' }));
    await waitFor(() => expect(mutates()).toHaveLength(2));
    expect(mutates()[1]).toMatchObject({
      baseLayoutRevision: '4',
      ops: [{ op: 'close_tile', tile: 't2' }],
    });
  });

  it('shows a failed watch even with no snapshot', async () => {
    const { layoutPort } = await mount(saved(null, {}));
    await waitFor(() => expect(layoutPort.listenerCount()).toBe(1));
    await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
    act(() =>
      layoutPort.emit({ kind: 'error', hostId: HOST_A, request: '1' as DecimalU64, code: 'unknown_space', message: 'no such space' }),
    );
    expect((await screen.findByRole('alert')).textContent).toBe('no such space');
    expect(document.querySelector('[data-tile-id]')).toBeNull();
  });
});

describe('App with tile IDs that name inherited properties', () => {
  // A client-state file as the backend sends it. An object literal's `__proto__:` sets its
  // prototype instead of a key, so the text is written out by hand.
  const stored = parseClientState(
    JSON.parse(
      `{"version":1,"lastSpace":{"spaceId":"${PATH}","selectedAtMs":"5"},"error":null,` +
        `"spaces":{"${PATH}":{"spaceId":"${PATH}","focusedTile":"__proto__","activeTerminal":null,` +
        '"activeTabs":{"constructor":"p2"},"groupState":{}}}}',
    ),
  );

  function inherited(): LayoutSnapshot {
    return {
      ...layout(),
      root: {
        type: 'split',
        dir: 'h',
        ratio: 0.5,
        a: { type: 'tile', id: '__proto__', panels: [], active: null },
        b: {
          type: 'split',
          dir: 'v',
          ratio: 0.5,
          a: { type: 'tile', id: 'constructor', panels: ['p1', 'p2'], active: 'p1' },
          b: { type: 'tile', id: 'toString', panels: [], active: null },
        },
      },
    };
  }

  const tileOf = (id: string) =>
    within(document.querySelector(`[data-tile-id="${id}"]`) as HTMLElement);

  it('restores, renders, splits and saves them', async () => {
    const { layoutPort, clientStatePort } = await mount(stored);
    await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
    act(() => layoutPort.push(HOST_A, inherited()));
    await waitFor(() => expect(focused()).toEqual(['__proto__']));
    expect(tileOf('__proto__').getByRole('tabpanel').textContent).toBe('No panels');
    expect(tileOf('toString').getByRole('tabpanel').textContent).toBe('No panels');
    expect(selectedTab('constructor')).toBe('p2');

    const mutates = () => layoutPort.calls.filter((call) => call.kind === 'mutate');
    fireEvent.click(tileOf('__proto__').getByRole('button', { name: 'Split down' }));
    await waitFor(() => expect(mutates()).toHaveLength(1));
    expect(mutates()[0]).toMatchObject({
      baseLayoutRevision: '3',
      ops: [{ op: 'split_tile', tile: '__proto__', dir: 'v', newTile: 't-new', ratio: 0.5, panel: null }],
    });

    fireEvent.click(tileOf('constructor').getByRole('tab', { name: 'p1' }));
    await waitFor(() => expect(selectedTab('constructor')).toBe('p1'));
    const write = clientStatePort.writes.filter((w) => w.kind === 'space').at(-1);
    if (write?.kind !== 'space') throw new Error('expected a space write');
    expect(write.path).toBe(PATH);
    expect(JSON.parse(JSON.stringify(write.state.activeTabs))).toEqual({ constructor: 'p1' });
  });
});
