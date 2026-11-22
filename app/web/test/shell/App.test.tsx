import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { type ClientState, defaultClientState } from '../../src/model/clientState';
import type { DecimalU64, HostId, TermId } from '../../src/model/ids';
import type { HostsPort } from '../../src/ipc/port';
import { App } from '../../src/shell/App';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
import { HOST_A, HOST_B, host, snapshot, space, terminal } from '../support/snapshots';

const connecting = { kind: 'connecting' } as const;

describe('App', () => {
  it('keeps the newest revision', async () => {
    const port = new MemoryHostsPort(snapshot([host('r10', null, connecting)], '10'));
    render(<App port={port} clientStatePort={new MemoryClientStatePort()} />);
    await screen.findByRole('region', { name: 'r10' });
    act(() => port.push(snapshot([host('r9', null, connecting)], '9')));
    expect(screen.queryByRole('region', { name: 'r9' })).toBeNull();
    act(() => port.push(snapshot([host('r11', null, connecting)], '11')));
    expect(screen.getByRole('region', { name: 'r11' })).toBeTruthy();
    act(() => port.push(snapshot([host('r2', null, connecting)], '2')));
    expect(screen.queryByRole('region', { name: 'r2' })).toBeNull();
    expect(screen.getByRole('region', { name: 'r11' })).toBeTruthy();
  });

  it('orders revisions above 2^53 exactly', async () => {
    const port = new MemoryHostsPort(snapshot([host('a', null, connecting)], '9007199254740992'));
    render(<App port={port} clientStatePort={new MemoryClientStatePort()} />);
    await screen.findByRole('region', { name: 'a' });
    act(() => port.push(snapshot([host('b', null, connecting)], '9007199254740993')));
    expect(screen.getByRole('region', { name: 'b' })).toBeTruthy();
    act(() => port.push(snapshot([host('c', null, connecting)], '9007199254740992')));
    expect(screen.queryByRole('region', { name: 'c' })).toBeNull();
    expect(screen.getByRole('region', { name: 'b' })).toBeTruthy();
  });

  it('subscribes before it fetches, so a snapshot pushed in between is kept', async () => {
    const calls: string[] = [];
    const inner = new MemoryHostsPort(snapshot([host('old', null, connecting)], '1'));
    const port: HostsPort = {
      onSnapshot: async (listener) => {
        calls.push('subscribe');
        const stop = await inner.onSnapshot(listener);
        inner.push(snapshot([host('pushed', null, connecting)], '5'));
        return stop;
      },
      getSnapshot: () => {
        calls.push('get');
        return Promise.resolve(snapshot([host('stale', null, connecting)], '3'));
      },
      retryHost: () => Promise.resolve(),
    };
    render(<App port={port} clientStatePort={new MemoryClientStatePort()} />);
    await screen.findByRole('region', { name: 'pushed' });
    expect(calls).toEqual(['subscribe', 'get']);
    expect(screen.queryByRole('region', { name: 'stale' })).toBeNull();
  });

  it('unsubscribes on unmount', async () => {
    const port = new MemoryHostsPort(snapshot([host('a', null, connecting)]));
    const view = render(<App port={port} clientStatePort={new MemoryClientStatePort()} />);
    await screen.findByRole('region', { name: 'a' });
    expect(port.listenerCount()).toBe(1);
    view.unmount();
    expect(port.listenerCount()).toBe(0);
  });

  it('Retry calls the port', async () => {
    const port = new MemoryHostsPort(
      snapshot([host('mini', null, { kind: 'failed', message: 'nope', detail: null })]),
    );
    render(<App port={port} clientStatePort={new MemoryClientStatePort()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Retry' }));
    expect(port.retryCalls).toEqual(['mini']);
  });

  it('shows an error when the backend cannot be reached', async () => {
    const port: HostsPort = {
      getSnapshot: () => Promise.reject(new Error('boom')),
      onSnapshot: () => Promise.resolve(() => undefined),
      retryHost: () => Promise.resolve(),
    };
    render(<App port={port} clientStatePort={new MemoryClientStatePort()} />);
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('Cannot reach the Combine backend: boom'));
  });

  describe('client state', () => {
    const connected = { kind: 'connected' } as const;

    function sample() {
      return snapshot([
        host('mac', HOST_A, connected, [
          space(HOST_A, '/src/alpha', [terminal('t1', 'first'), terminal('t2', 'second')]),
        ]),
        host('mini', HOST_B, connected, [
          space(HOST_B, '/src/mimir', [terminal('t3', 'remote'), terminal('t4', 'other')]),
        ]),
      ]);
    }

    function saved(
      path: string,
      at: string,
      activeTerminal: string | null = null,
    ): ClientState {
      return {
        ...defaultClientState(),
        lastSpace: { spaceId: path, selectedAtMs: at as DecimalU64 },
        spaces:
          activeTerminal === null
            ? {}
            : {
                [path]: {
                  spaceId: path,
                  focusedTile: null,
                  activeTerminal: activeTerminal as TermId,
                  activeTabs: {},
                  groupState: {},
                },
              },
      };
    }

    const current = () => document.querySelectorAll('nav [aria-current="true"]');

    it('restores last active space after restart', async () => {
      const states = new Map<HostId, ClientState>([
        [HOST_A, saved('/src/alpha', '9007199254740992')],
        [HOST_B, saved('/src/mimir', '9007199254740993')],
      ]);
      render(<App port={new MemoryHostsPort(sample())} clientStatePort={new MemoryClientStatePort(states)} />);
      await waitFor(() => expect(current()).toHaveLength(1));
      expect(current()[0]?.textContent).toBe('mimir');
      expect(document.querySelector('[data-term-id="t3"]')).not.toBeNull();
    });

    it('ignores a last space that is no longer listed', async () => {
      const states = new Map<HostId, ClientState>([
        [HOST_A, saved('/src/alpha', '5')],
        [HOST_B, saved('/src/removed', '9')],
      ]);
      render(<App port={new MemoryHostsPort(sample())} clientStatePort={new MemoryClientStatePort(states)} />);
      await waitFor(() => expect(current()[0]?.textContent).toBe('alpha'));
    });

    it('restores active terminal', async () => {
      const states = new Map<HostId, ClientState>([[HOST_A, saved('/src/alpha', '1', 't2')]]);
      const clientStatePort = new MemoryClientStatePort(states);
      render(<App port={new MemoryHostsPort(sample())} clientStatePort={clientStatePort} />);
      await waitFor(() => expect(document.querySelector('[data-term-id="t2"][aria-current="true"]')).not.toBeNull());
      expect(document.querySelector('[data-term-id="t1"][aria-current="true"]')).toBeNull();
      expect(clientStatePort.writes).toEqual([]);
    });

    it('drops a stored active terminal the host no longer has', async () => {
      const states = new Map<HostId, ClientState>([[HOST_A, saved('/src/alpha', '1', 't9')]]);
      const clientStatePort = new MemoryClientStatePort(states);
      render(<App port={new MemoryHostsPort(sample())} clientStatePort={clientStatePort} />);
      await waitFor(() => expect(current()[0]?.textContent).toBe('alpha'));
      expect(document.querySelector('[data-term-id][aria-current="true"]')).toBeNull();
      expect(clientStatePort.writes).toEqual([]);
    });

    it('keeps each space its own active terminal', async () => {
      const states = new Map<HostId, ClientState>([
        [HOST_A, saved('/src/alpha', '1', 't2')],
        [HOST_B, saved('/src/mimir', '2', 't4')],
      ]);
      render(<App port={new MemoryHostsPort(sample())} clientStatePort={new MemoryClientStatePort(states)} />);
      fireEvent.click(await screen.findByRole('button', { name: 'All spaces' }));
      await waitFor(() => expect(document.querySelectorAll('[data-term-id][aria-current="true"]')).toHaveLength(2));
      expect(document.querySelector('[data-term-id="t2"][aria-current="true"]')).not.toBeNull();
      expect(document.querySelector('[data-term-id="t4"][aria-current="true"]')).not.toBeNull();
    });

    it('invalid client state shows error and uses defaults', async () => {
      const states = new Map<HostId, ClientState>([
        [HOST_A, { ...defaultClientState('client state file is not valid JSON'), lastSpace: null }],
      ]);
      const clientStatePort = new MemoryClientStatePort(states);
      render(<App port={new MemoryHostsPort(sample())} clientStatePort={clientStatePort} />);
      const alert = await screen.findByRole('alert');
      expect(alert.textContent).toContain('Client state for mac: client state file is not valid JSON');
      expect(current()).toHaveLength(0);
      fireEvent.click(screen.getByRole('button', { name: 'alpha' }));
      expect(current()[0]?.textContent).toBe('alpha');
      expect(clientStatePort.writes).toEqual([]);
    });

    it('selecting writes that host state immediately, stamped by now', async () => {
      let tick = 1000;
      const clientStatePort = new MemoryClientStatePort();
      render(
        <App
          port={new MemoryHostsPort(sample())}
          clientStatePort={clientStatePort}
          now={() => ++tick}
        />,
      );
      fireEvent.click(await screen.findByRole('button', { name: 'mimir' }));
      expect(clientStatePort.writes).toEqual([{ kind: 'last', hostId: HOST_B, spaceId: '/src/mimir', selectedAtMs: '1001' }]);
      fireEvent.click(document.querySelector('[data-term-id="t4"] button') as Element);
      expect(clientStatePort.writes).toHaveLength(3);
      expect(clientStatePort.writes[1]).toMatchObject({
        kind: 'space',
        hostId: HOST_B,
        path: '/src/mimir',
        state: { activeTerminal: 't4' },
      });
      expect(clientStatePort.writes[2]).toMatchObject({ kind: 'last', selectedAtMs: '1002' });
      expect(document.querySelector('[data-term-id="t4"][aria-current="true"]')).not.toBeNull();
    });
  });
});
