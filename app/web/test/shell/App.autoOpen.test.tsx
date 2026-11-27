import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';
import { type ClientState, defaultClientState } from '../../src/model/clientState';
import type { DecimalU64, HostId, TermId } from '../../src/model/ids';
import { App } from '../../src/shell/App';
import { DeferredClientStatePort } from '../support/deferredClientStatePort';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryLayoutPort, defaultLayoutSnapshot } from '../support/memoryLayoutPort';
import { MemorySpawnPort } from '../support/memorySpawnPort';
import { MemoryTerminalPort } from '../support/memoryTerminalPort';
import { fakeXtermFactory } from '../support/fakeXterm';
import { HOST_A, host, snapshot, space, terminal } from '../support/snapshots';
import { installTerminalDom } from '../support/terminalDom';

const connected = { kind: 'connected' } as const;

function hosts(beta = [] as ReturnType<typeof terminal>[], revision = '1') {
  return snapshot(
    [
      host('mac', HOST_A, connected, [
        space(HOST_A, '/src/alpha', [
          terminal('t1', 'gone', 'sh', 'exited'),
          terminal('t2', 'second'),
          terminal('t3', 'third'),
        ]),
        space(HOST_A, '/src/beta', beta),
      ]),
    ],
    revision,
  );
}

function stored(activeTerminal: string | null): ClientState {
  return {
    ...defaultClientState(),
    spaces:
      activeTerminal === null
        ? {}
        : {
            '/src/alpha': {
              spaceId: '/src/alpha',
              focusedTile: null,
              activeTerminal: activeTerminal as TermId,
              activeTabs: {},
              groupState: {},
            },
          },
  };
}

const active = () =>
  [...document.querySelectorAll('[data-term-id][aria-current="true"]')].map((node) =>
    node.getAttribute('data-term-id'),
  );
const spaceWrites = (port: MemoryClientStatePort | DeferredClientStatePort) =>
  port.writes.filter((write) => write.kind === 'space');
const lastWrites = (port: MemoryClientStatePort | DeferredClientStatePort) =>
  port.writes.filter((write) => write.kind === 'last');

function mount(state: ClientState = stored(null), layoutPort = new MemoryLayoutPort()) {
  const spawn = new MemorySpawnPort();
  const hostsPort = new MemoryHostsPort(hosts());
  const clientStatePort = new MemoryClientStatePort(new Map<HostId, ClientState>([[HOST_A, state]]));
  render(
    <App
      port={hostsPort}
      clientStatePort={clientStatePort}
      layoutPort={layoutPort}
      spawnPort={spawn}
      terminalPort={new MemoryTerminalPort()}
      createAdapter={fakeXtermFactory().factory}
      now={() => 7}
    />,
  );
  return { spawn, hostsPort, clientStatePort };
}

const rail = async (name: string) => {
  const button = await screen.findByRole('button', { name });
  await act(async () => {});
  fireEvent.click(button);
};

function expectNoSpawn(spawn: MemorySpawnPort) {
  expect(spawn.spawnCalls()).toHaveLength(0);
  expect(spawn.listCalls()).toHaveLength(0);
}

describe('App auto-open', () => {
  it('selecting_a_space_activates_its_first_terminal_and_persists_it', async () => {
    const { spawn, clientStatePort } = mount();
    await rail('alpha');
    await waitFor(() => expect(active()).toEqual(['t2']));
    expect(spaceWrites(clientStatePort)).toEqual([
      expect.objectContaining({
        hostId: HOST_A,
        path: '/src/alpha',
        state: expect.objectContaining({ activeTerminal: 't2' }),
      }),
    ]);
    expect(lastWrites(clientStatePort)).toEqual([
      { kind: 'last', hostId: HOST_A, spaceId: '/src/alpha', selectedAtMs: '7' },
    ]);
    expectNoSpawn(spawn);
  });

  it('stored_valid_terminal_wins_and_nothing_is_written', async () => {
    const { spawn, clientStatePort } = mount(stored('t3'));
    await rail('alpha');
    await waitFor(() => expect(active()).toEqual(['t3']));
    expect(spaceWrites(clientStatePort)).toEqual([]);
    expect(lastWrites(clientStatePort)).toHaveLength(1);
    expectNoSpawn(spawn);
  });

  it('no_terminals_means_none_active_until_one_arrives', async () => {
    const { spawn, hostsPort, clientStatePort } = mount();
    await rail('beta');
    await screen.findByText('No terminals in this space.');
    expect(active()).toEqual([]);
    expect(spaceWrites(clientStatePort)).toEqual([]);
    act(() => hostsPort.push(hosts([terminal('t5', 'late')], '2')));
    await waitFor(() => expect(active()).toEqual(['t5']));
    expect(spaceWrites(clientStatePort)).toHaveLength(1);
    expect(lastWrites(clientStatePort)).toHaveLength(1);
    expectNoSpawn(spawn);
  });

  it('waits_for_client_state_to_load', async () => {
    const spawn = new MemorySpawnPort();
    const clientStatePort = new DeferredClientStatePort();
    render(
      <App
        port={new MemoryHostsPort(hosts())}
        clientStatePort={clientStatePort}
        layoutPort={new MemoryLayoutPort()}
        spawnPort={spawn}
        terminalPort={new MemoryTerminalPort()}
        createAdapter={fakeXtermFactory().factory}
        now={() => 7}
      />,
    );
    await waitFor(() => expect(clientStatePort.loads).toHaveLength(1));
    fireEvent.click(await screen.findByRole('button', { name: 'alpha' }));
    await act(async () => {});
    expect(active()).toEqual([]);
    expect(clientStatePort.writes).toEqual([]);
    await act(async () => {
      clientStatePort.resolve(HOST_A, stored('t3'));
    });
    await waitFor(() => expect(lastWrites(clientStatePort)).toHaveLength(1));
    expect(active()).toEqual(['t3']);
    expect(spaceWrites(clientStatePort)).toEqual([]);
    expectNoSpawn(spawn);
  });

  it('invalid_client_state_auto_opens_in_memory_only', async () => {
    const { spawn, clientStatePort } = mount({
      ...defaultClientState('client state file is not valid JSON'),
      lastSpace: { spaceId: '/x', selectedAtMs: '1' as DecimalU64 },
    });
    await screen.findByText(/client state file is not valid JSON/);
    await rail('alpha');
    await waitFor(() => expect(active()).toEqual(['t2']));
    expect(clientStatePort.writes).toEqual([]);
    expectNoSpawn(spawn);
  });

  it('auto_open_writes_once_per_repair_and_settles', async () => {
    // A stored terminal that is no longer listed is replaced once; later snapshots (same list, an
    // exited-only list, a list with the stored terminal back) never rewrite what is valid.
    const { spawn, hostsPort, clientStatePort } = mount(stored('t-gone'));
    await rail('alpha');
    await waitFor(() => expect(active()).toEqual(['t2']));
    await act(async () => {});
    expect(spaceWrites(clientStatePort)).toHaveLength(1);
    act(() => hostsPort.push(hosts([], '2')));
    await act(async () => {});
    expect(spaceWrites(clientStatePort)).toHaveLength(1);
    expect(active()).toEqual(['t2']);
    expectNoSpawn(spawn);
  });

  describe('a change that changes nothing', () => {
    beforeEach(() => installTerminalDom());

    async function shown() {
      const layoutPort = new MemoryLayoutPort();
      const rig = mount(stored(null), layoutPort);
      await rail('alpha');
      await waitFor(() => expect(layoutPort.listenerCount()).toBe(1));
      await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
      act(() => layoutPort.push(HOST_A, defaultLayoutSnapshot('/src/alpha')));
      await waitFor(() => expect(document.querySelector('[data-tile-id="main"]')).not.toBeNull());
      await waitFor(() => expect(active()).toEqual(['t2']));
      return rig;
    }

    it('a_space_change_that_changes_nothing_writes_nothing', async () => {
      const { clientStatePort } = await shown();
      const tile = document.querySelector('[data-tile-id="main"]') as Element;
      fireEvent.click(tile);
      await act(async () => {});
      const written = spaceWrites(clientStatePort).length;
      expect(clientStatePort.writes.filter((write) => write.kind === 'space' && write.state.focusedTile === 'main')).toHaveLength(1);
      fireEvent.click(tile);
      fireEvent.click(tile);
      await act(async () => {});
      expect(spaceWrites(clientStatePort)).toHaveLength(written);
    });

    it('selecting_the_stored_tab_writes_nothing', async () => {
      const { clientStatePort } = await shown();
      const tab = document.querySelector('[role="tab"][data-panel-id=".active-terminal"]') as Element;
      fireEvent.click(tab);
      await act(async () => {});
      const written = spaceWrites(clientStatePort).length;
      fireEvent.click(tab);
      await act(async () => {});
      expect(spaceWrites(clientStatePort)).toHaveLength(written);
    });

    it('activating_the_stored_terminal_writes_nothing', async () => {
      const { spawn, clientStatePort } = mount(stored('t3'));
      spawn.setSpawned('t3' as TermId);
      await rail('alpha');
      await waitFor(() => expect(active()).toEqual(['t3']));
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Add terminal' }));
      });
      await act(async () => {
        fireEvent.click(screen.getByRole('menuitem', { name: /^Shell/ }));
      });
      await act(async () => {});
      expect(spawn.spawnCalls()).toHaveLength(1);
      expect(spaceWrites(clientStatePort)).toEqual([]);
    });
  });
});
