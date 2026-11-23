import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { type ClientState, defaultClientState } from '../../src/model/clientState';
import type { DecimalU64, HostId } from '../../src/model/ids';
import { App } from '../../src/shell/App';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryLayoutPort } from '../support/memoryLayoutPort';
import { MemorySpawnPort } from '../support/memorySpawnPort';
import { HOST_A, host, snapshot, space, terminal } from '../support/snapshots';

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
});
