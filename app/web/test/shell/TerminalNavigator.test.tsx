import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
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
      space(HOST_A, '/src/zed', [terminal('t3', 'last')]),
      space(HOST_A, '/src/alpha', [
        terminal('t2', 'second', 'cmd two', 'running'),
        terminal('t1', 'first', 'cmd one', 'waiting'),
      ]),
      space(HOST_A, '/src/empty'),
    ]),
    host('mini', HOST_B, { kind: 'reconnecting', attempt: 1 }, [
      space(HOST_B, '/src/mimir', [terminal('t4', 'remote')]),
    ]),
    host('bare', null, { kind: 'connecting' }),
  ]);
}

async function mount() {
  render(<App terminalPort={new MemoryTerminalPort()} layoutPort={new MemoryLayoutPort()} port={new MemoryHostsPort(sample())} clientStatePort={new MemoryClientStatePort()} />);
  await screen.findByRole('navigation', { name: 'Spaces' });
}

const spaceButton = (host: string, name: string) =>
  within(screen.getByRole('region', { name: host })).getByRole('button', { name });
const navigator = () => screen.getByRole('region', { name: 'Terminals' });

describe('TerminalNavigator', () => {
  it('prompts to select a space', async () => {
    await mount();
    expect(within(navigator()).getByText('Select a space to see its terminals.')).toBeTruthy();
  });

  it("lists the selected space's terminals with name, command, state", async () => {
    await mount();
    fireEvent.click(spaceButton('mac', 'alpha'));
    const rows = [...navigator().querySelectorAll('li')];
    expect(rows.map((r) => r.getAttribute('data-term-id'))).toEqual(['t2', 't1']);
    expect(rows[0]?.querySelector('.name')?.textContent).toBe('second');
    expect(rows[0]?.querySelector('.command')?.textContent).toBe('cmd two');
    expect(rows[0]?.querySelector('.state')?.textContent).toBe('running');
    expect(rows[0]?.querySelector('button')?.getAttribute('aria-label')).toBe('second, cmd two, running');
    fireEvent.click(spaceButton('mac', 'empty'));
    expect(within(navigator()).getByText('No terminals in this space.')).toBeTruthy();
  });

  it('All spaces groups by host then space', async () => {
    await mount();
    fireEvent.click(within(navigator()).getByRole('button', { name: 'All spaces' }));
    expect(within(navigator()).getByRole('button', { name: 'All spaces' }).getAttribute('aria-pressed')).toBe('true');
    const labels = [...navigator().querySelectorAll('section')].map((s) => s.getAttribute('aria-label'));
    expect(labels).toEqual(['mac', 'alpha', 'zed', 'mini', 'mimir']);
    const ids = [...navigator().querySelectorAll('li')].map((r) => r.getAttribute('data-term-id'));
    expect(ids).toEqual(['t2', 't1', 't3', 't4']);
  });

  it('toggle returns to the selected space', async () => {
    await mount();
    fireEvent.click(spaceButton('mac', 'zed'));
    const toggle = within(navigator()).getByRole('button', { name: 'All spaces' });
    fireEvent.click(toggle);
    fireEvent.click(toggle);
    expect(toggle.getAttribute('aria-pressed')).toBe('false');
    expect([...navigator().querySelectorAll('li')].map((r) => r.getAttribute('data-term-id'))).toEqual(['t3']);
  });

  it('dims rows of a host that is not connected', async () => {
    await mount();
    fireEvent.click(within(navigator()).getByRole('button', { name: 'All spaces' }));
    const dim = (id: string) => navigator().querySelector(`[data-term-id="${id}"]`)?.getAttribute('data-dimmed');
    expect(dim('t1')).toBe('false');
    expect(dim('t4')).toBe('true');
  });

  it('prompts again when the selected space disappears', async () => {
    const port = new MemoryHostsPort(sample());
    render(<App terminalPort={new MemoryTerminalPort()} layoutPort={new MemoryLayoutPort()} port={port} clientStatePort={new MemoryClientStatePort()} />);
    await screen.findByRole('navigation', { name: 'Spaces' });
    fireEvent.click(spaceButton('mac', 'alpha'));
    expect(navigator().querySelectorAll('li')).toHaveLength(2);
    act(() =>
      port.push(
        snapshot([host('mac', HOST_A, connected, [space(HOST_A, '/src/zed', [terminal('t3', 'last')])])], '2'),
      ),
    );
    expect(within(navigator()).getByText('Select a space to see its terminals.')).toBeTruthy();
  });

  it('shows No terminals when nothing remains', async () => {
    render(<App terminalPort={new MemoryTerminalPort()} layoutPort={new MemoryLayoutPort()} port={new MemoryHostsPort(snapshot([host('a', HOST_A, connected, [space(HOST_A, '/x')])]))} clientStatePort={new MemoryClientStatePort()} />);
    await screen.findByRole('navigation', { name: 'Spaces' });
    fireEvent.click(within(navigator()).getByRole('button', { name: 'All spaces' }));
    expect(within(navigator()).getByText('No terminals.')).toBeTruthy();
  });
});
