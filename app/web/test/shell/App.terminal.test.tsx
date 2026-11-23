import { act, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';
import { type ClientState, defaultClientState } from '../../src/model/clientState';
import type { DecimalU64, HostId, TermId } from '../../src/model/ids';
import { App } from '../../src/shell/App';
import { fakeXtermFactory } from '../support/fakeXterm';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryLayoutPort } from '../support/memoryLayoutPort';
import {
  MemoryTerminalPort,
  attachedEvent,
  connectionEvent,
  cursor,
  ownershipEvent,
  startedEvent,
} from '../support/memoryTerminalPort';
import { HOST_A, host, snapshot, space, terminal } from '../support/snapshots';
import { FakeResizeObserver, installTerminalDom } from '../support/terminalDom';

const PATH = '/src/alpha';

beforeEach(() => {
  installTerminalDom();
});

describe('App terminal mount', () => {
  it('mounts the stored active terminal as a TerminalPanel with its label', async () => {
    const state: ClientState = {
      ...defaultClientState(),
      lastSpace: { spaceId: PATH, selectedAtMs: '5' as DecimalU64 },
      spaces: {
        [PATH]: {
          spaceId: PATH,
          focusedTile: null,
          activeTerminal: 'term-1' as TermId,
          activeTabs: {},
          groupState: {},
        },
      },
    };
    const hosts = snapshot([
      host('mac', HOST_A, { kind: 'connected' }, [
        space(HOST_A, PATH, [terminal('term-1', 'build', 'make', 'running')]),
      ]),
    ]);
    const terminalPort = new MemoryTerminalPort();
    const layoutPort = new MemoryLayoutPort();
    render(
      <App
        port={new MemoryHostsPort(hosts)}
        clientStatePort={new MemoryClientStatePort(new Map<HostId, ClientState>([[HOST_A, state]]))}
        layoutPort={layoutPort}
        terminalPort={terminalPort}
        createAdapter={fakeXtermFactory().factory}
      />,
    );
    await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
    await waitFor(() => expect(layoutPort.listenerCount()).toBe(1));
    act(() =>
      layoutPort.push(HOST_A, {
        space: PATH,
        layoutRevision: '1' as DecimalU64,
        root: { type: 'tile', id: 't1', panels: [], active: null },
        readOnly: false,
        error: null,
        dropped: null,
      }),
    );
    const panel = await screen.findByTestId('terminal-panel');
    expect(panel.hidden).toBe(false);
    expect(panel.textContent).toContain('build');
    expect(panel.textContent).toContain('mac');
    await act(async () => {});
    await waitFor(() => expect(terminalPort.streams).toHaveLength(1));
  });
  it('keeps the panel mounted through a host drop, and the reconnect resumes the same stream', async () => {
    const state: ClientState = {
      ...defaultClientState(),
      lastSpace: { spaceId: PATH, selectedAtMs: '5' as DecimalU64 },
      spaces: {
        [PATH]: {
          spaceId: PATH,
          focusedTile: null,
          activeTerminal: 'term-1' as TermId,
          activeTabs: {},
          groupState: {},
        },
      },
    };
    const connected = snapshot([
      host('mac', HOST_A, { kind: 'connected' }, [
        space(HOST_A, PATH, [terminal('term-1', 'build', 'make', 'running')]),
      ]),
    ]);
    const hostsPort = new MemoryHostsPort(connected);
    const terminalPort = new MemoryTerminalPort();
    const layoutPort = new MemoryLayoutPort();
    render(
      <App
        port={hostsPort}
        clientStatePort={new MemoryClientStatePort(new Map<HostId, ClientState>([[HOST_A, state]]))}
        layoutPort={layoutPort}
        terminalPort={terminalPort}
        createAdapter={fakeXtermFactory().factory}
      />,
    );
    await waitFor(() => expect(layoutPort.listenerCount()).toBe(1));
    await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
    act(() =>
      layoutPort.push(HOST_A, {
        space: PATH,
        layoutRevision: '1' as DecimalU64,
        root: { type: 'tile', id: 't1', panels: [], active: null },
        readOnly: false,
        error: null,
        dropped: null,
      }),
    );
    await screen.findByTestId('terminal-panel');
    await waitFor(() => expect(terminalPort.streams).toHaveLength(1));
    const container = document.querySelector('.terminal-view');
    if (container === null) throw new Error('no terminal view');
    act(() => FakeResizeObserver.resize(container, 800, 340));
    await act(async () => {});
    const stream = terminalPort.streams[0];
    if (stream === undefined) throw new Error('stream not open');
    act(() => {
      stream.emit(attachedEvent(1, 7));
      stream.emit(startedEvent(cursor(7, 1, 0)));
      stream.emit(ownershipEvent(7, 1, 8));
    });
    await act(async () => {});

    act(() => hostsPort.setConnection('mac', { kind: 'reconnecting', attempt: 1 }));
    act(() => layoutPort.emit({ kind: 'disconnected', hostId: HOST_A }));
    act(() => stream.emit(connectionEvent(false)));
    await act(async () => {});
    expect(screen.getByTestId('terminal-panel')).toBeTruthy();
    expect(stream.commands('close')).toHaveLength(0);
    expect(stream.commands('detach')).toHaveLength(0);

    act(() => hostsPort.setConnection('mac', { kind: 'connected' }));
    act(() => stream.emit(connectionEvent(true)));
    await act(async () => {});
    expect(stream.commands('resume')).toHaveLength(1);
    expect(stream.commands('attach')).toHaveLength(1);
    expect(terminalPort.streams).toHaveLength(1);
    expect(stream.commands('close')).toHaveLength(0);
  });
});
