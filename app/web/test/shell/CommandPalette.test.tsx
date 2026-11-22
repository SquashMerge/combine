import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { HostsPort } from '../../src/ipc/port';
import { App } from '../../src/shell/App';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { HOST_A, HOST_B, host, snapshot, space, terminal } from '../support/snapshots';

const connected = { kind: 'connected' } as const;

function sample() {
  return snapshot([
    host('mac', HOST_A, connected, [
      space(HOST_A, '/src/alpha', [terminal('t1', 'first', 'cmd one')]),
    ]),
    host('mini', HOST_B, { kind: 'failed', message: 'boom', detail: null }, [
      space(HOST_B, '/src/mimir', [terminal('t2', 'remote', 'cmd two')]),
    ]),
  ]);
}

async function mount(port: HostsPort = new MemoryHostsPort(sample())) {
  render(<App port={port} />);
  await screen.findByRole('navigation', { name: 'Spaces' });
}

const open = () => fireEvent.keyDown(window, { key: 'k', metaKey: true });
const dialog = () => screen.queryByRole('dialog', { name: 'Command palette' });
const search = () => screen.getByRole('combobox');
const options = () => within(screen.getByRole('listbox')).getAllByRole('option');

describe('CommandPalette', () => {
  it('Cmd+K opens and Escape closes', async () => {
    await mount();
    expect(dialog()).toBeNull();
    fireEvent.keyDown(window, { key: 'k' });
    expect(dialog()).toBeNull();
    open();
    expect(dialog()).not.toBeNull();
    expect(document.activeElement).toBe(search());
    fireEvent.keyDown(search(), { key: 'Escape' });
    expect(dialog()).toBeNull();
  });

  it('filters by fuzzy match', async () => {
    await mount();
    open();
    fireEvent.change(search(), { target: { value: 'mimr' } });
    expect(options().map((o) => o.querySelector('.label')?.textContent)).toEqual(['mimir', 'remote']);
    fireEvent.change(search(), { target: { value: 'zzzz' } });
    expect(screen.queryAllByRole('option')).toHaveLength(0);
  });

  it('moves with Up and Down', async () => {
    await mount();
    open();
    expect(options()[0]?.getAttribute('aria-selected')).toBe('true');
    fireEvent.keyDown(search(), { key: 'ArrowDown' });
    expect(options()[1]?.getAttribute('aria-selected')).toBe('true');
    fireEvent.keyDown(search(), { key: 'ArrowUp' });
    fireEvent.keyDown(search(), { key: 'ArrowUp' });
    expect(options()[0]?.getAttribute('aria-selected')).toBe('true');
  });

  it('Enter on space selects like the rail', async () => {
    await mount();
    open();
    fireEvent.change(search(), { target: { value: 'mimir' } });
    fireEvent.keyDown(search(), { key: 'Enter' });
    expect(dialog()).toBeNull();
    const rail = within(screen.getByRole('region', { name: 'mini' })).getByRole('button', { name: 'mimir' });
    expect(rail.getAttribute('aria-current')).toBe('true');
  });

  it('Enter on terminal selects like the navigator', async () => {
    await mount();
    open();
    fireEvent.change(search(), { target: { value: 'remote' } });
    expect(options()[0]?.getAttribute('data-palette-entry')).toBe('focus-terminal');
    fireEvent.keyDown(search(), { key: 'Enter' });
    const rail = within(screen.getByRole('region', { name: 'mini' })).getByRole('button', { name: 'mimir' });
    expect(rail.getAttribute('aria-current')).toBe('true');
    const item = document.querySelector('[data-term-id="t2"]');
    expect(item?.getAttribute('aria-current')).toBe('true');
  });

  it('navigator click marks the active terminal', async () => {
    await mount();
    fireEvent.click(within(screen.getByRole('region', { name: 'mac' })).getByRole('button', { name: 'alpha' }));
    const nav = screen.getByRole('region', { name: 'Terminals' });
    fireEvent.click(within(nav).getByRole('button', { name: /first/ }));
    expect(document.querySelector('[data-term-id="t1"]')?.getAttribute('aria-current')).toBe('true');
  });

  it('retry calls retryHost with name', async () => {
    const retryHost = vi.fn(() => Promise.resolve());
    const inner = new MemoryHostsPort(sample());
    await mount({
      getSnapshot: () => inner.getSnapshot(),
      onSnapshot: (listener) => inner.onSnapshot(listener),
      retryHost,
    });
    open();
    fireEvent.change(search(), { target: { value: 'retry' } });
    expect(options()).toHaveLength(1);
    act(() => {
      fireEvent.keyDown(search(), { key: 'Enter' });
    });
    expect(retryHost).toHaveBeenCalledExactlyOnceWith('mini');
    expect(dialog()).toBeNull();
  });
});
