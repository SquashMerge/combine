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
