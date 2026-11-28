import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { HostsPort } from '../../src/ipc/port';
import { CommandPalette } from '../../src/shell/CommandPalette';
import { App } from '../../src/shell/App';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryLayoutPort } from '../support/memoryLayoutPort';
import { MemoryTerminalPort } from '../support/memoryTerminalPort';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
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
  render(<App terminalPort={new MemoryTerminalPort()} layoutPort={new MemoryLayoutPort()} port={port} clientStatePort={new MemoryClientStatePort()} />);
  await screen.findByRole('navigation', { name: 'Spaces' });
  await act(async () => {});
}

const codesOf = (option: HTMLElement) =>
  Array.from(option.querySelectorAll('.detail code')).map((code) => code.textContent);

const open = async () => {
  fireEvent.keyDown(window, { key: 'k', metaKey: true });
  await screen.findByRole('dialog', { name: 'Command palette' });
};
const dialog = () => screen.queryByRole('dialog', { name: 'Command palette' });
const search = () => screen.getByRole('combobox');
const options = () => within(screen.getByRole('listbox')).getAllByRole('option');

describe('CommandPalette', () => {
  it('Cmd+K opens and Escape closes', async () => {
    await mount();
    expect(dialog()).toBeNull();
    fireEvent.keyDown(window, { key: 'k' });
    expect(dialog()).toBeNull();
    await open();
    expect(dialog()).not.toBeNull();
    expect(document.activeElement).toBe(search());
    fireEvent.keyDown(search(), { key: 'Escape' });
    expect(dialog()).toBeNull();
  });

  it('marks paths, folders and commands in each detail', async () => {
    await mount();
    await open();
    const byLabel = (label: string) =>
      options().find((o) => o.querySelector('.label')?.textContent === label) as HTMLElement;
    const space = byLabel('alpha');
    expect(space.querySelector('.detail')?.textContent).toBe('mac /src/alpha');
    expect(codesOf(space)).toEqual(['/src/alpha']);
    expect(codesOf(byLabel('first'))).toEqual(['alpha', 'cmd one']);
  });

  it('filters by fuzzy match', async () => {
    await mount();
    await open();
    fireEvent.change(search(), { target: { value: 'mimr' } });
    expect(options().map((o) => o.querySelector('.label')?.textContent)).toEqual(['mimir', 'remote']);
    fireEvent.change(search(), { target: { value: 'zzzz' } });
    expect(screen.queryAllByRole('option')).toHaveLength(0);
  });

  it('moves with Up and Down', async () => {
    await mount();
    await open();
    expect(options()[0]?.getAttribute('aria-selected')).toBe('true');
    fireEvent.keyDown(search(), { key: 'ArrowDown' });
    expect(options()[1]?.getAttribute('aria-selected')).toBe('true');
    fireEvent.keyDown(search(), { key: 'ArrowUp' });
    fireEvent.keyDown(search(), { key: 'ArrowUp' });
    expect(options()[0]?.getAttribute('aria-selected')).toBe('true');
  });

  it('Enter on space selects like the rail', async () => {
    await mount();
    await open();
    fireEvent.change(search(), { target: { value: 'mimir' } });
    fireEvent.keyDown(search(), { key: 'Enter' });
    expect(dialog()).toBeNull();
    const rail = within(screen.getByRole('region', { name: 'mini' })).getByRole('button', { name: 'mimir' });
    expect(rail.getAttribute('aria-current')).toBe('true');
  });

  it('Enter on terminal selects like the navigator', async () => {
    await mount();
    await open();
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
    await open();
    fireEvent.change(search(), { target: { value: 'retry' } });
    expect(options()).toHaveLength(1);
    act(() => {
      fireEvent.keyDown(search(), { key: 'Enter' });
    });
    expect(retryHost).toHaveBeenCalledExactlyOnceWith('mini');
    expect(dialog()).toBeNull();
  });

  describe('returns focus to the element focused before it opened', () => {
    const rail = () => screen.getByRole('button', { name: 'alpha' });

    async function openFromRail() {
      await mount();
      rail().focus();
      expect(document.activeElement).toBe(rail());
      await open();
      expect(document.activeElement).toBe(search());
    }

    it('after Escape', async () => {
      await openFromRail();
      fireEvent.keyDown(search(), { key: 'Escape' });
      expect(dialog()).toBeNull();
      expect(document.activeElement).toBe(rail());
    });

    it('after a second Cmd+K', async () => {
      await openFromRail();
      fireEvent.keyDown(window, { key: 'k', metaKey: true });
      expect(dialog()).toBeNull();
      expect(document.activeElement).toBe(rail());
    });

    it('after Enter', async () => {
      await openFromRail();
      fireEvent.keyDown(search(), { key: 'Enter' });
      expect(dialog()).toBeNull();
      expect(document.activeElement).toBe(rail());
    });
  });

  it('Tab and Shift+Tab stay in the search field', async () => {
    await mount();
    await open();
    for (const shiftKey of [false, true]) {
      const notPrevented = fireEvent.keyDown(search(), { key: 'Tab', shiftKey });
      expect(notPrevented).toBe(false);
      expect(document.activeElement).toBe(search());
    }
  });

  it('the field has aria-autocomplete=list', async () => {
    await mount();
    await open();
    expect(search().getAttribute('aria-autocomplete')).toBe('list');
    expect(search().getAttribute('aria-label')).toBe('Search spaces, terminals and panels');
  });

  it('a backdrop click closes and a click inside does not', async () => {
    await mount();
    await open();
    fireEvent.click(search());
    expect(dialog()).not.toBeNull();
    fireEvent.click(screen.getByRole('dialog', { name: 'Command palette' }));
    expect(dialog()).not.toBeNull();
    fireEvent.click(dialog()!.parentElement!);
    expect(dialog()).toBeNull();
  });

  it('lists panels and runs the panel handlers when given them', async () => {
    const space = { hostId: HOST_A, path: '/src/alpha' };
    const onClosePanel = vi.fn();
    render(
      <CommandPalette
        snapshot={sample()}
        panels={{
          space,
          hostName: 'mac',
          panels: [],
          tiles: new Map(),
          focused: { tile: 't', panel: 'notes' },
          editable: true,
        }}
        onSelectSpace={() => {}}
        onSelectTerminal={() => {}}
        onRetryHost={() => {}}
        onClosePanel={onClosePanel}
      />,
    );
    await open();
    fireEvent.change(search(), { target: { value: 'close panel' } });
    expect(options()[0]?.getAttribute('data-palette-entry')).toBe('close-panel');
    fireEvent.keyDown(search(), { key: 'Enter' });
    expect(onClosePanel).toHaveBeenCalledExactlyOnceWith(space, 'notes');
  });
});
