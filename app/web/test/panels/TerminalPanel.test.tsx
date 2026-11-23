import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parseTermId } from '../../src/model/ids';
import { TerminalPanel } from '../../src/panels/TerminalPanel';
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

async function mount(visible = true): Promise<Mounted> {
  const port = new MemoryTerminalPort();
  const fake = fakeXtermFactory();
  const element = (show: boolean) => (
    <TerminalPanel
      port={port}
      hostId={HOST_ID}
      termId={TERM}
      visible={show}
      label={label}
      createAdapter={fake.factory}
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
      { op: 'requestOwnership', attachment: '7' },
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
});
