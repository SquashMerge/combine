import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { type ClientState, defaultClientState } from '../../src/model/clientState';
import type { DecimalU64, HostId, TermId } from '../../src/model/ids';
import type { LayoutNode, LayoutSnapshot } from '../../src/model/layout';
import { terminalTabId } from '../../src/model/terminalTabs';
import { App } from '../../src/shell/App';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryLayoutPort } from '../support/memoryLayoutPort';
import { MemoryPanelPort } from '../support/memoryPanelPort';
import { MemorySpawnPort } from '../support/memorySpawnPort';
import { MemoryTerminalPort } from '../support/memoryTerminalPort';
import { fakeXtermFactory } from '../support/fakeXterm';
import { HOST_A, host, snapshot, space, terminal } from '../support/snapshots';
import { installTerminalDom } from '../support/terminalDom';

const connected = { kind: 'connected' } as const;

function hosts(extra = false, connection: Parameters<typeof host>[2] = connected, revision = '1') {
  return snapshot(
    [
      host('mac', HOST_A, connection, [
        space(HOST_A, '/src/alpha', [terminal('t1', 'first'), ...(extra ? [terminal('t2', 'second')] : [])]),
        space(HOST_A, '/src/beta', [terminal('t3', 'third')]),
      ]),
    ],
    revision,
  );
}

async function mount(state: ClientState = defaultClientState()) {
  const spawn = new MemorySpawnPort();
  const hostsPort = new MemoryHostsPort(hosts());
  const clientStatePort = new MemoryClientStatePort(new Map<HostId, ClientState>([[HOST_A, state]]));
  render(
    <App
      port={hostsPort}
      clientStatePort={clientStatePort}
      layoutPort={new MemoryLayoutPort()}
      spawnPort={spawn}
      terminalPort={new MemoryTerminalPort()}
      createAdapter={fakeXtermFactory().factory}
    />,
  );
  await screen.findByRole('button', { name: 'alpha' });
  await act(async () => {});
  return { spawn, hostsPort };
}

const none = (spawn: MemorySpawnPort) => {
  expect(spawn.spawnCalls()).toHaveLength(0);
  expect(spawn.listCalls()).toHaveLength(0);
};

async function palette(entry: string) {
  fireEvent.keyDown(window, { key: 'k', metaKey: true });
  const list = await screen.findByRole('listbox');
  fireEvent.click(within(list).getAllByRole('option').find((o) => o.getAttribute('data-palette-entry') === entry) as Element);
}

describe('App never spawns on its own', () => {
  it('selecting_spaces_and_terminals_never_spawns', async () => {
    const { spawn } = await mount();
    fireEvent.click(screen.getByRole('button', { name: 'beta' }));
    await palette('switch-space');
    await palette('focus-terminal');
    fireEvent.click(document.querySelector('[data-term-id] button') as Element);
    await act(async () => {});
    none(spawn);
  });

  it('restoring_last_space_never_spawns', async () => {
    const { spawn } = await mount({
      ...defaultClientState(),
      lastSpace: { spaceId: '/src/beta', selectedAtMs: '5' as DecimalU64 },
    });
    await waitFor(() => expect(document.querySelector('nav [aria-current="true"]')?.textContent).toBe('beta'));
    none(spawn);
  });

  it('auto_open_never_spawns', async () => {
    const { spawn } = await mount();
    fireEvent.click(screen.getByRole('button', { name: 'alpha' }));
    await waitFor(() =>
      expect(document.querySelector('[data-term-id="t1"][aria-current="true"]')).not.toBeNull(),
    );
    none(spawn);
  });

  it('host_reconnect_never_spawns', async () => {
    const { spawn, hostsPort } = await mount();
    fireEvent.click(screen.getByRole('button', { name: 'alpha' }));
    await act(async () => {});
    act(() => hostsPort.push(hosts(false, { kind: 'failed', message: 'dropped', detail: null }, '2')));
    act(() => hostsPort.push(hosts(true, connected, '3')));
    await act(async () => {});
    none(spawn);
  });

  it('terminal_list_changes_never_spawn', async () => {
    const { spawn, hostsPort } = await mount();
    fireEvent.click(screen.getByRole('button', { name: 'alpha' }));
    act(() => hostsPort.push(hosts(true, connected, '2')));
    act(() => hostsPort.push(hosts(false, connected, '3')));
    await act(async () => {});
    none(spawn);
  });

  it('closing_the_menu_without_a_click_never_spawns', async () => {
    const { spawn } = await mount();
    fireEvent.click(screen.getByRole('button', { name: 'alpha' }));
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Add terminal' }));
    });
    expect(spawn.listCalls()).toHaveLength(1);
    fireEvent.keyDown(document.body, { key: 'Escape' });
    await act(async () => {});
    expect(screen.queryByRole('menu')).toBeNull();
    expect(spawn.spawnCalls()).toHaveLength(0);
  });

  it('terminal_tabs_never_spawn', async () => {
    installTerminalDom();
    const pinned = (id: string) => terminalTabId('core-a', id as TermId) as string;
    const spawn = new MemorySpawnPort();
    const layoutPort = new MemoryLayoutPort();
    const panelPort = new MemoryPanelPort();
    const state: ClientState = {
      ...defaultClientState(),
      lastSpace: { spaceId: '/src/alpha', selectedAtMs: '5' as DecimalU64 },
    };
    render(
      <App
        port={
          new MemoryHostsPort(
            snapshot([
              host('mac', HOST_A, connected, [space(HOST_A, '/src/alpha', [terminal('t1', 'first'), terminal('t2', 'second')])], {
                id: 'herdr-core',
                instance: 'core-a',
              }),
            ]),
          )
        }
        clientStatePort={new MemoryClientStatePort(new Map<HostId, ClientState>([[HOST_A, state]]))}
        layoutPort={layoutPort}
        panelPort={panelPort}
        spawnPort={spawn}
        terminalPort={new MemoryTerminalPort()}
        createAdapter={fakeXtermFactory().factory}
        newTileId={() => 't-new'}
      />,
    );
    await waitFor(() => expect(layoutPort.listenerCount()).toBe(1));
    await waitFor(() => expect(panelPort.listenerCount()).toBe(1));
    await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
    let revision = 1;
    const push = (root: LayoutSnapshot['root'], request: string | null = null) => {
      revision += 1;
      act(() =>
        layoutPort.push(
          HOST_A,
          { space: '/src/alpha', layoutRevision: String(revision) as DecimalU64, root, readOnly: false, error: null, dropped: null },
          request as DecimalU64 | null,
        ),
      );
    };
    // Restored tabs: a follow-active tab and a pinned one.
    push({
      type: 'split',
      dir: 'h',
      ratio: 0.5,
      a: { type: 'tile', id: 'a', panels: ['.active-terminal'], active: '.active-terminal' },
      b: { type: 'tile', id: 'b', panels: [pinned('t1')], active: pinned('t1') },
    });
    act(() => panelPort.emit({ kind: 'list', hostId: HOST_A, request: null, list: { space: '/src/alpha', panels: [], skipped: [], error: null } }));
    await screen.findAllByTestId('terminal-panel');
    // A follow-active switch.
    const switchers = screen.getAllByTestId('terminal-switcher');
    fireEvent.change(switchers[0] as Element, { target: { value: 't2' } });
    // Re-pin (request 2).
    fireEvent.change(switchers[1] as Element, { target: { value: 't2' } });
    await act(async () => {});
    const a: LayoutNode = { type: 'tile', id: 'a', panels: ['.active-terminal'], active: '.active-terminal' };
    const pair = (b: LayoutSnapshot['root']): LayoutSnapshot['root'] => ({ type: 'split', dir: 'h', ratio: 0.5, a, b });
    push(pair({ type: 'tile', id: 'b', panels: [pinned('t2')], active: pinned('t2') }), '2');
    fireEvent.click(document.querySelector('[data-tile-id="b"]') as Element);
    // Navigator open-beside (request 3).
    fireEvent.click(document.querySelector('[data-term-id="t1"] button') as Element);
    await act(async () => {});
    push(
      pair({
        type: 'split',
        dir: 'h',
        ratio: 0.5,
        a: { type: 'tile', id: 'b', panels: [pinned('t2')], active: pinned('t2') },
        b: { type: 'tile', id: 't-new', panels: [pinned('t1')], active: pinned('t1') },
      }),
      '3',
    );
    // Close a pinned tab (request 4).
    fireEvent.click(screen.getByRole('button', { name: 'Close panel first' }));
    await act(async () => {});
    push(pair({ type: 'tile', id: 'b', panels: [pinned('t2')], active: pinned('t2') }), '4');
    // The palette action (request 5).
    fireEvent.click(document.querySelector('[data-tile-id="b"]') as Element);
    await palette('show-active-terminal');
    await act(async () => {});
    expect(layoutPort.calls.filter((call) => call.kind === 'mutate').length).toBe(4);
    none(spawn);
  });
});
