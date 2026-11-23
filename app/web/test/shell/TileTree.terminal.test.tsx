import { act, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { parseTermId } from '../../src/model/ids';
import type { LayoutSnapshot } from '../../src/model/layout';
import { TileTree, type TerminalMount } from '../../src/shell/TileTree';
import { fakeXtermFactory } from '../support/fakeXterm';
import {
  HOST_ID,
  MemoryTerminalPort,
  attachedEvent,
  closedEvent,
  cursor,
  ownershipEvent,
  startedEvent,
} from '../support/memoryTerminalPort';
import { FakeResizeObserver, installTerminalDom } from '../support/terminalDom';

const TERM = parseTermId('term-1');
const MOUNT: TerminalMount = {
  hostId: HOST_ID,
  termId: TERM,
  label: { name: 'build', host: 'mini', state: 'running' },
};

function layout(): LayoutSnapshot {
  return {
    space: '/work',
    layoutRevision: '1' as LayoutSnapshot['layoutRevision'],
    root: {
      type: 'split',
      dir: 'h',
      ratio: 0.5,
      a: { type: 'tile', id: 't1', panels: ['p1'], active: 'p1' },
      b: { type: 'tile', id: 't2', panels: [], active: null },
    },
    readOnly: false,
    error: null,
    dropped: null,
  };
}

async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

function tree(port: MemoryTerminalPort, fake: ReturnType<typeof fakeXtermFactory>, focusedTile: string, terminal: TerminalMount | null) {
  return (
    <TileTree
      layout={layout()}
      notice={null}
      error={null}
      focusedTile={focusedTile}
      activeTabs={{}}
      onFocus={vi.fn()}
      onSelectTab={vi.fn()}
      onSplit={vi.fn()}
      onClose={vi.fn()}
      terminalPort={port}
      terminal={terminal}
      createAdapter={fake.factory}
    />
  );
}

beforeEach(() => {
  installTerminalDom();
});

describe('TileTree terminal mount', () => {
  it('the active terminal renders a visible TerminalPanel in the focused tile', async () => {
    const port = new MemoryTerminalPort();
    const fake = fakeXtermFactory();
    render(tree(port, fake, 't1', MOUNT));
    await flush();
    const panel = screen.getByTestId('terminal-panel');
    expect(panel.hidden).toBe(false);
    expect(document.querySelector('[data-tile-id="t1"]')?.contains(panel)).toBe(true);
    expect(screen.getByText('build')).toBeTruthy();
    expect(screen.getByText('mini')).toBeTruthy();
    expect(port.streams).toHaveLength(1);
  });

  it('renders no panel in an unfocused tile or without an active terminal', async () => {
    const port = new MemoryTerminalPort();
    const fake = fakeXtermFactory();
    const view = render(tree(port, fake, 't1', null));
    await flush();
    expect(screen.queryByTestId('terminal-panel')).toBeNull();
    view.rerender(tree(port, fake, 't2', MOUNT));
    await flush();
    expect(document.querySelector('[data-tile-id="t2"]')?.querySelector('[data-testid="terminal-panel"]')).not.toBeNull();
    expect(document.querySelector('[data-tile-id="t1"]')?.querySelector('[data-testid="terminal-panel"]')).toBeNull();
  });

  it('unmounting a panel stops its controller: detach is sent and its stream closed', async () => {
    const port = new MemoryTerminalPort();
    const fake = fakeXtermFactory();
    const view = render(tree(port, fake, 't1', MOUNT));
    await flush();
    const container = document.querySelector('.terminal-view');
    if (container === null) throw new Error('no terminal view');
    act(() => FakeResizeObserver.resize(container, 800, 340));
    await flush();
    const stream = port.streams[0];
    if (stream === undefined) throw new Error('stream not open');
    act(() => {
      stream.emit(attachedEvent(1, 7));
      stream.emit(startedEvent(cursor(7, 1, 0)));
      stream.emit(ownershipEvent(7, 1, 8));
    });
    await flush();
    expect(stream.commands('detach')).toHaveLength(0);
    expect(stream.commands('close')).toHaveLength(0);
    view.rerender(tree(port, fake, 't1', null));
    await flush();
    expect(stream.commands('close')).toHaveLength(0);
    act(() => stream.emit(closedEvent(cursor(7, 1, 1), 'detached')));
    await flush();
    expect(screen.queryByTestId('terminal-panel')).toBeNull();
    expect(stream.commands('detach')).toEqual([{ op: 'detach', attachment: '7' }]);
    expect(stream.commands('close')).toHaveLength(1);
    expect(fake.instances[0]?.disposed).toBe(true);
  });
});
