import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { HostsPort } from '../../src/ipc/port';
import { App } from '../../src/shell/App';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { host, snapshot } from '../support/snapshots';

const connecting = { kind: 'connecting' } as const;

describe('App', () => {
  it('keeps the newest revision', async () => {
    const port = new MemoryHostsPort(snapshot([host('r10', null, connecting)], '10'));
    render(<App port={port} />);
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
    render(<App port={port} />);
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
    render(<App port={port} />);
    await screen.findByRole('region', { name: 'pushed' });
    expect(calls).toEqual(['subscribe', 'get']);
    expect(screen.queryByRole('region', { name: 'stale' })).toBeNull();
  });

  it('unsubscribes on unmount', async () => {
    const port = new MemoryHostsPort(snapshot([host('a', null, connecting)]));
    const view = render(<App port={port} />);
    await screen.findByRole('region', { name: 'a' });
    expect(port.listenerCount()).toBe(1);
    view.unmount();
    expect(port.listenerCount()).toBe(0);
  });

  it('Retry calls the port', async () => {
    const port = new MemoryHostsPort(
      snapshot([host('mini', null, { kind: 'failed', message: 'nope', detail: null })]),
    );
    render(<App port={port} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Retry' }));
    expect(port.retryCalls).toEqual(['mini']);
  });

  it('shows an error when the backend cannot be reached', async () => {
    const port: HostsPort = {
      getSnapshot: () => Promise.reject(new Error('boom')),
      onSnapshot: () => Promise.resolve(() => undefined),
      retryHost: () => Promise.resolve(),
    };
    render(<App port={port} />);
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('Cannot reach the Combine backend: boom'));
  });
});
