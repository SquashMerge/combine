import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { TermId } from '../../src/model/ids';
import { App } from '../../src/shell/App';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryLayoutPort } from '../support/memoryLayoutPort';
import { MemorySpawnPort } from '../support/memorySpawnPort';
import { MemoryTerminalPort } from '../support/memoryTerminalPort';
import { fakeXtermFactory } from '../support/fakeXterm';
import { HOST_A, host, snapshot, space, terminal } from '../support/snapshots';

const connected = { kind: 'connected' } as const;
const PATH = '/src/alpha';

function hosts(terminals: ReturnType<typeof terminal>[], revision = '1') {
  return snapshot([host('mac', HOST_A, connected, [space(HOST_A, PATH, terminals)])], revision);
}

const active = () =>
  [...document.querySelectorAll('[data-term-id][aria-current="true"]')].map((node) =>
    node.getAttribute('data-term-id'),
  );

async function mount(terminals: ReturnType<typeof terminal>[]) {
  const spawn = new MemorySpawnPort();
  spawn.setSpawned('t9' as TermId);
  const hostsPort = new MemoryHostsPort(hosts(terminals));
  const clientStatePort = new MemoryClientStatePort();
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
  const space = await screen.findByRole('button', { name: 'alpha' });
  await act(async () => {});
  fireEvent.click(space);
  return { spawn, hostsPort, clientStatePort };
}

async function addShell() {
  await act(async () => {
    fireEvent.click(screen.getByRole('button', { name: 'Add terminal' }));
  });
  await act(async () => {
    fireEvent.click(screen.getByRole('menuitem', { name: /^Shell/ }));
  });
}

const spaceWrites = (port: MemoryClientStatePort) =>
  port.writes.flatMap((write) =>
    write.kind === 'space' ? [write.state.activeTerminal] : [],
  );

describe('App Add terminal', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('pending_activation_expires_after_ten_seconds', async () => {
    const { spawn, hostsPort, clientStatePort } = await mount([terminal('t1', 'first')]);
    await waitFor(() => expect(active()).toEqual(['t1']));
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    await addShell();
    expect(spawn.spawnCalls()).toHaveLength(1);
    await act(async () => {
      vi.advanceTimersByTime(10_000);
    });
    act(() => hostsPort.push(hosts([terminal('t1', 'first'), terminal('t9', 'new')], '2')));
    await act(async () => {});
    expect(active()).toEqual(['t1']);
    expect(spaceWrites(clientStatePort)).toEqual(['t1']);
  });

  it('pending_activation_still_live_just_before_ten_seconds', async () => {
    const { hostsPort } = await mount([terminal('t1', 'first')]);
    await waitFor(() => expect(active()).toEqual(['t1']));
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    await addShell();
    await act(async () => {
      vi.advanceTimersByTime(9_999);
    });
    act(() => hostsPort.push(hosts([terminal('t1', 'first'), terminal('t9', 'new')], '2')));
    await act(async () => {});
    vi.useRealTimers();
    await waitFor(() => expect(active()).toEqual(['t9']));
  });

  it('spawned_terminal_becomes_active_once_listed', async () => {
    const { spawn, hostsPort, clientStatePort } = await mount([terminal('t1', 'first')]);
    await waitFor(() => expect(active()).toEqual(['t1']));
    await addShell();
    expect(spawn.spawnCalls()).toHaveLength(1);
    expect(active()).toEqual(['t1']);
    act(() => hostsPort.push(hosts([terminal('t1', 'first'), terminal('t9', 'new')], '2')));
    await waitFor(() => expect(active()).toEqual(['t9']));
    expect(spaceWrites(clientStatePort)).toEqual(['t1', 't9']);
  });

  it('overrides_auto_open_pick', async () => {
    const { hostsPort, clientStatePort } = await mount([]);
    await screen.findByText('No terminals in this space.');
    await addShell();
    act(() => hostsPort.push(hosts([terminal('t1', 'first'), terminal('t9', 'new')], '2')));
    await waitFor(() => expect(active()).toEqual(['t9']));
    expect(spaceWrites(clientStatePort)).toEqual(['t9']);
  });

  it('activation_adds_no_spawn', async () => {
    const { spawn, hostsPort } = await mount([terminal('t1', 'first')]);
    await addShell();
    act(() => hostsPort.push(hosts([terminal('t1', 'first'), terminal('t9', 'new')], '2')));
    await waitFor(() => expect(active()).toEqual(['t9']));
    act(() => hostsPort.push(hosts([terminal('t9', 'new')], '3')));
    await act(async () => {});
    expect(spawn.spawnCalls()).toHaveLength(1);
    expect(spawn.listCalls()).toHaveLength(1);
  });
});
