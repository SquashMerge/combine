import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parseTermId } from '../../src/model/ids';
import { GoneTerminal } from '../../src/panels/GoneTerminal';
import { goneMarked, goneText } from '../../src/panels/panelText';
import { TerminalPanel, type TerminalPanelProps } from '../../src/panels/TerminalPanel';
import { fakeXtermFactory } from '../support/fakeXterm';
import {
  HOST_ID,
  MemoryTerminalPort,
  attachedEvent,
  closedEvent,
  cursor,
  gapEvent,
  lostEvent,
  ownershipEvent,
  startedEvent,
  type MemoryTerminalStream,
} from '../support/memoryTerminalPort';
import { FakeResizeObserver, installTerminalDom } from '../support/terminalDom';

const TERM = parseTermId('term-1');
const label = { name: 'build', host: 'mini', state: 'running' };

async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

interface Mounted {
  port: MemoryTerminalPort;
  fake: ReturnType<typeof fakeXtermFactory>;
  rerender(visible: boolean): void;
  stream(): MemoryTerminalStream;
  emit(...events: Parameters<MemoryTerminalStream['emit']>[0][]): Promise<void>;
}

async function mount(
  visible = true,
  extra: Partial<TerminalPanelProps> = {},
  port = new MemoryTerminalPort(),
): Promise<Mounted> {
  const fake = fakeXtermFactory();
  const element = (show: boolean) => (
    <TerminalPanel
      port={port}
      hostId={HOST_ID}
      termId={TERM}
      visible={show}
      label={label}
      createAdapter={fake.factory}
      {...extra}
    />
  );
  const view = render(element(visible));
  await flush();
  const container = document.querySelector('.terminal-view');
  if (container === null) throw new Error('no terminal view');
  act(() => FakeResizeObserver.resize(container, 800, 340));
  const stream = (): MemoryTerminalStream => {
    const first = port.streams[0];
    if (first === undefined) throw new Error('stream not open');
    return first;
  };
  return {
    port,
    fake,
    rerender: (show) => view.rerender(element(show)),
    stream,
    async emit(...events) {
      act(() => {
        for (const event of events) stream().emit(event);
      });
      await flush();
    },
  };
}

async function goLive(t: Mounted, owner: number | null = 8): Promise<void> {
  await t.emit(
    attachedEvent(1, 7),
    startedEvent(cursor(7, 1, 0)),
    ownershipEvent(7, 1, owner),
  );
}

beforeEach(() => {
  installTerminalDom();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('TerminalPanel', () => {
  it('header shows name, state, host and the ownership mark', async () => {
    const t = await mount();
    expect(screen.getByText('build')).toBeTruthy();
    expect(screen.getByText('running')).toBeTruthy();
    expect(screen.getByText('mini')).toBeTruthy();
    expect(screen.getByText('input: unknown')).toBeTruthy();
    await goLive(t, 8);
    expect(screen.getByText('input: another panel')).toBeTruthy();
    await t.emit(ownershipEvent(7, 2, 7));
    expect(screen.getByText('input: this panel')).toBeTruthy();
    await t.emit(ownershipEvent(7, 3, null));
    expect(screen.getByText('input: none')).toBeTruthy();
  });

  it('take input button sends request_ownership', async () => {
    const t = await mount();
    const button = screen.getByRole('button', { name: 'Take input' }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    await goLive(t, 8);
    expect(button.disabled).toBe(false);
    fireEvent.click(button);
    expect(t.stream().commands('requestOwnership')).toEqual([
      { op: 'requestOwnership', attachment: '7', ifVacant: null },
    ]);
    await t.emit(ownershipEvent(7, 2, 7));
    expect(button.disabled).toBe(true);
  });

  it('lost status offers reattach', async () => {
    const t = await mount();
    expect(screen.queryByRole('button', { name: 'Reattach' })).toBeNull();
    await goLive(t);
    await t.emit(lostEvent(cursor(7, 1, 1)));
    expect(screen.getByTestId('terminal-status').textContent).toBe('lost');
    fireEvent.click(screen.getByRole('button', { name: 'Reattach' }));
    expect(t.stream().commands('attach')).toHaveLength(2);
  });

  it('closed status offers no reattach', async () => {
    const t = await mount();
    await goLive(t);
    await t.emit(closedEvent(cursor(7, 1, 1), 'exited'));
    expect(screen.getByTestId('terminal-status').textContent).toBe('exited');
    expect(screen.queryByRole('button', { name: 'Reattach' })).toBeNull();
  });

  it('shows the reconnecting overlay while stale', async () => {
    const t = await mount();
    await goLive(t);
    expect(screen.queryByRole('status')).toBeNull();
    await t.emit(gapEvent(7, null));
    expect(screen.getByRole('status').textContent).toBe('reconnecting');
    expect(document.querySelector('[data-dimmed="true"]')).not.toBeNull();
  });

  it('a hidden panel holds no xterm instance', async () => {
    const t = await mount();
    await goLive(t);
    expect(t.fake.instances).toHaveLength(1);
    expect(t.fake.instances[0]?.disposed).toBe(false);
    t.rerender(false);
    await flush();
    expect(t.fake.instances[0]?.disposed).toBe(true);
    const panel = screen.getByTestId('terminal-panel');
    expect(panel.hidden).toBe(true);
    expect(panel.querySelector('.terminal-view')).not.toBeNull();
  });

  it('retry after a refused stream', async () => {
    const port = new MemoryTerminalPort();
    port.failNextOpen('link down');
    const t = await mount(true, {}, port);
    expect(screen.getByTestId('terminal-status').textContent).toBe(
      "Couldn't open the terminal stream: link down",
    );
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await flush();
    expect(port.streams).toHaveLength(1);
    expect(screen.queryByRole('button', { name: 'Retry' })).toBeNull();
    expect(screen.queryByTestId('terminal-status')).toBeNull();
    await goLive(t, 8);
    expect(screen.getByText('input: another panel')).toBeTruthy();
  });

  it('no Retry button without a refused stream', async () => {
    await mount();
    expect(screen.queryByRole('button', { name: 'Retry' })).toBeNull();
  });

  it('header shows the backend label', async () => {
    await mount(true, { label: { ...label, backend: 'core-a' } });
    expect(screen.getByTestId('terminal-backend').textContent).toBe('core-a');
  });

  it('dropdown switches terminals', async () => {
    const onSwitch = vi.fn();
    await mount(true, {
      switcher: {
        options: [
          { id: 'term-1', label: 'build', disabled: false },
          { id: 'term-2', label: 'logs', disabled: false },
          { id: 'term-3', label: 'taken', disabled: true, title: 'Already open' },
        ],
        onSwitch,
      },
    });
    const select = screen.getByRole('combobox', { name: 'Terminal' }) as HTMLSelectElement;
    expect(select.value).toBe('term-1');
    const taken = screen.getByRole('option', { name: 'taken' }) as HTMLOptionElement;
    expect(taken.disabled).toBe(true);
    expect(taken.title).toBe('Already open');
    expect((screen.getByRole('option', { name: 'logs' }) as HTMLOptionElement).title).toBe('');
    fireEvent.change(select, { target: { value: 'term-2' } });
    expect(onSwitch).toHaveBeenCalledWith('term-2');
  });

  it('without the new props the panel renders as before', async () => {
    const t = await mount();
    await goLive(t, null);
    await t.emit(closedEvent(cursor(7, 1, 1), 'exited'));
    const panel = screen.getByTestId('terminal-panel');
    expect(panel.querySelector('.terminal-panel__backend')).toBeNull();
    expect(panel.querySelector('select')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Close' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Retry' })).toBeNull();
    expect(
      Array.from(panel.querySelectorAll('button')).map((button) => button.textContent),
    ).toEqual(['Take input']);
    expect(t.stream().commands('requestOwnership')).toEqual([]);
    expect(t.stream().commands('resize')).toEqual([]);
  });

  it('an unfocused panel never claims a terminal with no owner', async () => {
    const t = await mount();
    await goLive(t, null);
    expect(t.stream().commands('requestOwnership')).toEqual([]);
  });

  it('a focused panel claims a terminal with no owner', async () => {
    const t = await mount(true, { focused: true });
    await goLive(t, null);
    expect(t.stream().commands('requestOwnership')).toEqual([
      { op: 'requestOwnership', attachment: '7', ifVacant: '1' },
    ]);
  });

  it('close shows only when exited', async () => {
    const onClose = vi.fn();
    const t = await mount(true, { onClose });
    await goLive(t);
    expect(screen.queryByRole('button', { name: 'Close' })).toBeNull();
    await t.emit(closedEvent(cursor(7, 1, 1), 'exited'));
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('close shows for an exited label and disables with its title', async () => {
    await mount(true, {
      onClose: vi.fn(),
      label: { ...label, state: 'exited' },
      closeDisabledTitle: 'Layout is held',
    });
    const button = screen.getByRole('button', { name: 'Close' }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(button.title).toBe('Layout is held');
  });

  it('a lost terminal shows no Close', async () => {
    const t = await mount(true, { onClose: vi.fn() });
    await goLive(t);
    await t.emit(lostEvent(cursor(7, 1, 1)));
    expect(screen.queryByRole('button', { name: 'Close' })).toBeNull();
  });
});

describe('GoneTerminal', () => {
  it('marks the terminal ID in a marked message', () => {
    render(<GoneTerminal message={goneMarked('unlisted', 'term-9')} onClose={vi.fn()} />);
    const message = screen.getByTestId('gone-terminal-message');
    expect(message.textContent).toBe(goneText('unlisted', 'term-9'));
    expect(message.querySelector('code')?.textContent).toBe('term-9');
  });

  it('shows its message and a Close button, and attaches nothing', () => {
    const onClose = vi.fn();
    const port = new MemoryTerminalPort();
    render(<GoneTerminal message={goneText('unlisted', 'term-9')} onClose={onClose} />);
    expect(screen.getByTestId('gone-terminal-message').textContent).toBe(
      'Terminal term-9 is gone. The host no longer lists it.',
    );
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(port.streams).toEqual([]);
  });

  it('disables Close with its title', () => {
    render(<GoneTerminal message="x" onClose={vi.fn()} disabledTitle="Layout is held" />);
    const button = screen.getByRole('button', { name: 'Close' }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(button.title).toBe('Layout is held');
  });

  it('has the three exact texts', () => {
    expect(goneText('unlisted', 't')).toBe('Terminal t is gone. The host no longer lists it.');
    expect(goneText('other-instance', 't')).toBe(
      'Terminal t is gone. It belonged to a terminal backend this host no longer runs.',
    );
    expect(goneText('no-instance', 't')).toBe(
      "Terminal t can't be shown: the host doesn't say which terminal backend instance it runs.",
    );
  });
});
