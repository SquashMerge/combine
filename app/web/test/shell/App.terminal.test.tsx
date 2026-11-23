import { act, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';
import { type ClientState, defaultClientState } from '../../src/model/clientState';
import type { DecimalU64, HostId, TermId } from '../../src/model/ids';
import { App } from '../../src/shell/App';
import { fakeXtermFactory } from '../support/fakeXterm';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryLayoutPort } from '../support/memoryLayoutPort';
import { MemoryTerminalPort } from '../support/memoryTerminalPort';
import { HOST_A, host, snapshot, space, terminal } from '../support/snapshots';
import { installTerminalDom } from '../support/terminalDom';

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
});
