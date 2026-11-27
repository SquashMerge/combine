import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';
import { type ClientState, defaultClientState } from '../../src/model/clientState';
import type { BackendView, HostsSnapshot } from '../../src/model/hosts';
import type { DecimalU64, HostId, TermId } from '../../src/model/ids';
import type { LayoutNode, LayoutOp, LayoutSnapshot } from '../../src/model/layout';
import type { PanelSummary } from '../../src/model/panels';
import { ACTIVE_TERMINAL_TAB, terminalTabId } from '../../src/model/terminalTabs';
import { App } from '../../src/shell/App';
import { fakeXtermFactory } from '../support/fakeXterm';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryLayoutPort, defaultLayoutSnapshot } from '../support/memoryLayoutPort';
import { MemoryPanelPort } from '../support/memoryPanelPort';
import {
  MemoryTerminalPort,
  attachedEvent,
  connectionEvent,
  cursor,
  ownershipEvent,
  startedEvent,
} from '../support/memoryTerminalPort';
import { HOST_A, HOST_B, host, snapshot, space, terminal } from '../support/snapshots';
import { FakeResizeObserver, installTerminalDom } from '../support/terminalDom';

const PATH = '/src/alpha';
const INSTANCE = 'core-a';
const BACKEND: BackendView = { id: 'herdr-core', instance: INSTANCE };
const connected = { kind: 'connected' } as const;
const TERMINALS = [
  terminal('term-1', 'build', 'make', 'running'),
  terminal('term-2', 'deploy', 'ship', 'idle'),
  terminal('term-3', 'done', 'true', 'exited'),
];

const tab = (id: string): string => {
  const result = terminalTabId(INSTANCE, id as TermId);
  if (result === null) throw new Error('no tab ID');
  return result;
};
const PIN_1 = tab('term-1');
const PIN_2 = tab('term-2');
const PIN_3 = tab('term-3');

function hostsOf(
  backend: BackendView | null | undefined = BACKEND,
  terminals = TERMINALS,
  revision = '1',
): HostsSnapshot {
  return snapshot([host('mac', HOST_A, connected, [space(HOST_A, PATH, terminals)], backend)], revision);
}

function stored(over: Partial<ClientState['spaces'][string]> = {}): ClientState {
  return {
    ...defaultClientState(),
    lastSpace: { spaceId: PATH, selectedAtMs: '5' as DecimalU64 },
    spaces: {
      [PATH]: {
        spaceId: PATH,
        focusedTile: null,
        activeTerminal: 'term-1' as TermId,
        activeTabs: {},
        groupState: {},
        ...over,
      },
    },
  };
}

function tile(id: string, panels: string[], active: string | null = panels[0] ?? null): LayoutNode {
  return { type: 'tile', id, panels, active };
}

function split(a: LayoutNode, b: LayoutNode): LayoutNode {
  return { type: 'split', dir: 'h', ratio: 0.5, a, b };
}

function layoutOf(root: LayoutNode, revision = '1'): LayoutSnapshot {
  return {
    space: PATH,
    layoutRevision: revision as DecimalU64,
    root,
    readOnly: false,
    error: null,
    dropped: null,
  };
}

interface MountOptions {
  hosts?: HostsSnapshot;
  state?: ClientState;
  layout?: LayoutSnapshot;
  panelPort?: MemoryPanelPort;
}

async function mountApp({ hosts = hostsOf(), state = stored(), layout, panelPort }: MountOptions = {}) {
  const hostsPort = new MemoryHostsPort(hosts);
  const layoutPort = new MemoryLayoutPort();
  const terminalPort = new MemoryTerminalPort();
  const clientStatePort = new MemoryClientStatePort(new Map<HostId, ClientState>([[HOST_A, state]]));
  render(
    <App
      port={hostsPort}
      clientStatePort={clientStatePort}
      layoutPort={layoutPort}
      terminalPort={terminalPort}
      createAdapter={fakeXtermFactory().factory}
      newTileId={() => 't-new'}
      now={() => 9}
      {...(panelPort === undefined ? {} : { panelPort })}
    />,
  );
  await waitFor(() => expect(layoutPort.listenerCount()).toBe(1));
  if (panelPort !== undefined) await waitFor(() => expect(panelPort.listenerCount()).toBe(1));
  await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
  act(() => layoutPort.push(HOST_A, layout ?? defaultLayoutSnapshot(PATH)));
  await act(async () => {});
  return { hostsPort, layoutPort, terminalPort, clientStatePort };
}

const mutations = (layoutPort: MemoryLayoutPort): LayoutOp[][] =>
  layoutPort.calls.flatMap((call) => (call.kind === 'mutate' ? [call.ops] : []));
const spaceWrites = (port: MemoryClientStatePort) =>
  port.writes.flatMap((write) => (write.kind === 'space' ? [write.state] : []));
const lastWrites = (port: MemoryClientStatePort) => port.writes.filter((write) => write.kind === 'last');
const bodyOf = (id: string) => document.querySelector(`[data-tile-id="${id}"] .tile-body`) as HTMLElement;
const clickTerminal = async (id: string) => {
  fireEvent.click(document.querySelector(`[data-term-id="${id}"] button`) as Element);
  await act(async () => {});
};

beforeEach(() => {
  installTerminalDom();
});

describe('App terminal tabs', () => {
  it('follow_active_tab_shows_the_active_terminal_in_any_tile', async () => {
    const { terminalPort } = await mountApp({
      layout: layoutOf(split(tile('t1', ['p1']), tile('t2', [ACTIVE_TERMINAL_TAB]))),
      state: stored({ focusedTile: 't1' }),
    });
    const panel = await screen.findByTestId('terminal-panel');
    expect(document.querySelector('[data-tile-id="t2"]')?.contains(panel)).toBe(true);
    expect(panel.textContent).toContain('build');
    expect(panel.textContent).toContain('mac');
    expect(screen.getByTestId('terminal-backend').textContent).toBe('herdr-core');
    await waitFor(() => expect(terminalPort.streams).toHaveLength(1));
  });

  it('keeps the panel mounted through a host drop, and the reconnect resumes the same stream', async () => {
    const { hostsPort, layoutPort, terminalPort } = await mountApp();
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
    expect(terminalPort.streams).toHaveLength(1);
    expect(stream.commands('close')).toHaveLength(0);
  });

  it('during a host drop, selecting the same path on another host shows no held tree and no panel', async () => {
    const hosts = snapshot([
      host('mac', HOST_A, connected, [space(HOST_A, PATH, TERMINALS)], BACKEND),
      host('box', HOST_B, connected, [space(HOST_B, PATH, [])], BACKEND),
    ]);
    const { hostsPort, layoutPort } = await mountApp({
      hosts,
      layout: layoutOf(tile('held-tile', [ACTIVE_TERMINAL_TAB])),
    });
    await screen.findByTestId('terminal-panel');
    act(() => hostsPort.setConnection('mac', { kind: 'reconnecting', attempt: 1 }));
    act(() => layoutPort.emit({ kind: 'disconnected', hostId: HOST_A }));
    await act(async () => {});
    expect(document.querySelector('[data-tile-id="held-tile"]')).not.toBeNull();
    const boxSection = screen.getByRole('region', { name: 'box' });
    fireEvent.click(within(boxSection).getByRole('button', { name: /^alpha/ }));
    await act(async () => {});
    expect(document.querySelector('[data-tile-id="held-tile"]')).toBeNull();
    expect(screen.queryByTestId('terminal-panel')).toBeNull();
  });

  it('dropdown_on_the_follow_active_tab_changes_the_active_terminal', async () => {
    const { layoutPort, clientStatePort } = await mountApp();
    const select = await screen.findByTestId('terminal-switcher');
    fireEvent.change(select, { target: { value: 'term-2' } });
    await act(async () => {});
    expect(spaceWrites(clientStatePort).at(-1)?.activeTerminal).toBe('term-2');
    expect(document.querySelector('[data-term-id="term-2"][aria-current="true"]')).not.toBeNull();
    expect(screen.getByTestId('terminal-panel').textContent).toContain('deploy');
    expect(mutations(layoutPort)).toEqual([]);
  });

  it('dropdown_re_pins_a_pinned_tab', async () => {
    const { layoutPort } = await mountApp({ layout: layoutOf(tile('t1', [PIN_1])) });
    const select = await screen.findByTestId('terminal-switcher');
    fireEvent.change(select, { target: { value: 'term-2' } });
    await act(async () => {});
    expect(mutations(layoutPort)).toEqual([
      [
        { op: 'add_panel', panel: PIN_2, tile: 't1' },
        { op: 'remove_panel', panel: PIN_1 },
      ],
    ]);
  });

  it('an_already_open_terminal_is_disabled', async () => {
    await mountApp({ layout: layoutOf(split(tile('t1', [PIN_1]), tile('t2', [PIN_2]))) });
    await screen.findAllByTestId('terminal-panel');
    const select = within(bodyOf('t1')).getByTestId('terminal-switcher') as HTMLSelectElement;
    expect([...select.options].map((o) => [o.textContent, o.disabled])).toEqual([
      ['build (running)', false],
      ['deploy (already open)', true],
      ['done (exited)', false],
    ]);
  });

  it('pinned_tabs_come_back_after_a_reconnect', async () => {
    const { hostsPort, layoutPort, terminalPort } = await mountApp({
      layout: layoutOf(split(tile('t1', [PIN_1]), tile('t2', [PIN_2]))),
    });
    await screen.findAllByTestId('terminal-panel');
    await waitFor(() => expect(terminalPort.streams).toHaveLength(2));
    act(() => hostsPort.setConnection('mac', { kind: 'reconnecting', attempt: 1 }));
    act(() => layoutPort.emit({ kind: 'disconnected', hostId: HOST_A }));
    await act(async () => {});
    expect(screen.getAllByTestId('terminal-panel')).toHaveLength(2);
    expect(screen.queryByTestId('gone-terminal')).toBeNull();
    act(() => hostsPort.setConnection('mac', { kind: 'connected' }));
    act(() => layoutPort.push(HOST_A, layoutOf(split(tile('t1', [PIN_1]), tile('t2', [PIN_2])), '2')));
    await act(async () => {});
    expect(screen.getAllByTestId('terminal-panel')).toHaveLength(2);
    expect(terminalPort.streams).toHaveLength(2);
    for (const stream of terminalPort.streams) expect(stream.commands('close')).toHaveLength(0);
  });

  it('a_pinned_tab_no_longer_listed_reads_as_gone', async () => {
    const { hostsPort } = await mountApp({ layout: layoutOf(tile('t1', [PIN_2])) });
    await screen.findByTestId('terminal-panel');
    act(() => hostsPort.push(hostsOf(BACKEND, [TERMINALS[0] as (typeof TERMINALS)[number]], '2')));
    await act(async () => {});
    expect(screen.queryByTestId('terminal-panel')).toBeNull();
    expect(screen.getByTestId('gone-terminal-message').textContent).toBe(
      'Terminal term-2 is gone. The host no longer lists it.',
    );
  });

  it('a_pinned_tab_waits_before_the_hosts_first_terminal_list', async () => {
    const { terminalPort } = await mountApp({
      hosts: hostsOf(null),
      layout: layoutOf(tile('t1', [PIN_1])),
    });
    expect(bodyOf('t1').textContent).toBe("Waiting for the host's terminal list…");
    const close = screen.getByRole('button', { name: 'Close panel term-1' }) as HTMLButtonElement;
    expect(close.disabled).toBe(true);
    expect(close.title).toBe('Loading…');
    expect(terminalPort.streams).toHaveLength(0);
  });

  it('a_pinned_tab_of_another_backend_instance_reads_as_gone', async () => {
    const { hostsPort, terminalPort } = await mountApp({ layout: layoutOf(tile('t1', [PIN_1])) });
    await screen.findByTestId('terminal-panel');
    act(() => hostsPort.setBackend('mac', { id: 'herdr-core', instance: 'core-b' }));
    await act(async () => {});
    expect(screen.queryByTestId('terminal-panel')).toBeNull();
    expect(screen.getByTestId('gone-terminal-message').textContent).toBe(
      'Terminal term-1 is gone. It belonged to a terminal backend this host no longer runs.',
    );
    expect(terminalPort.streams).toHaveLength(1);
  });

  it('a_pinned_tab_of_a_host_without_an_instance_reads_as_gone', async () => {
    await mountApp({ hosts: hostsOf({ id: 'herdr-core', instance: null }), layout: layoutOf(tile('t1', [PIN_1])) });
    expect(screen.getByTestId('gone-terminal-message').textContent).toBe(
      "Terminal term-1 can't be shown: the host doesn't say which terminal backend instance it runs.",
    );
  });

  it('close_on_a_terminal_tab_sends_remove_panel_not_close_panel', async () => {
    const panelPort = new MemoryPanelPort();
    const { layoutPort } = await mountApp({
      panelPort,
      layout: layoutOf(split(tile('t1', [PIN_3]), tile('t2', ['.terminal/core-a/term-9']))),
    });
    // A listed panel that carries the pinned tab's ID would be closed with close_panel by a view tab.
    const lookalike: PanelSummary = {
      id: PIN_3,
      title: 'Lookalike',
      renderer: 'markdown',
      revision: '1' as DecimalU64,
      state: 'validated',
      error: null,
      createdBy: null,
      lifetime: 'persistent',
    };
    await act(async () => {
      panelPort.emit({ kind: 'list', hostId: HOST_A, request: null, list: { space: PATH, panels: [lookalike], skipped: [], error: null } });
    });
    fireEvent.click(screen.getByRole('button', { name: 'Close panel term-9' }));
    await act(async () => {});
    act(() => layoutPort.push(HOST_A, layoutOf(split(tile('t1', [PIN_3]), tile('t2', [])), '2'), '2' as DecimalU64));
    await act(async () => {});
    fireEvent.click(within(screen.getByTestId('terminal-panel')).getByRole('button', { name: 'Close' }));
    await act(async () => {});
    expect(mutations(layoutPort)).toEqual([
      [{ op: 'remove_panel', panel: '.terminal/core-a/term-9' }],
      [{ op: 'remove_panel', panel: PIN_3 }],
    ]);
    expect(panelPort.actions).toEqual([]);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('navigator_click_always_sets_the_active_terminal_and_last_space', async () => {
    const { layoutPort, clientStatePort } = await mountApp();
    await clickTerminal('term-2');
    expect(spaceWrites(clientStatePort).at(-1)?.activeTerminal).toBe('term-2');
    expect(lastWrites(clientStatePort)).toEqual([{ kind: 'last', hostId: HOST_A, spaceId: PATH, selectedAtMs: '9' }]);
    expect(mutations(layoutPort)).toEqual([]);
  });

  it('navigator_adds_to_an_empty_focused_tile', async () => {
    const { layoutPort, clientStatePort } = await mountApp({ layout: layoutOf(tile('t1', [])) });
    await clickTerminal('term-2');
    expect(mutations(layoutPort)).toEqual([[{ op: 'add_panel', panel: PIN_2, tile: 't1' }]]);
    expect(spaceWrites(clientStatePort).at(-1)?.activeTerminal).toBe('term-2');
    expect(lastWrites(clientStatePort)).toHaveLength(1);
  });

  it('navigator_opens_a_terminal_beside_a_pinned_tab', async () => {
    const { layoutPort, clientStatePort } = await mountApp({ layout: layoutOf(tile('t1', [PIN_1])) });
    await clickTerminal('term-2');
    expect(mutations(layoutPort)).toEqual([
      [
        { op: 'split_tile', tile: 't1', dir: 'h', newTile: 't-new', ratio: 0.5, panel: null },
        { op: 'add_panel', panel: PIN_2, tile: 't-new' },
      ],
    ]);
    expect(spaceWrites(clientStatePort).at(-1)?.activeTerminal).toBe('term-2');
    act(() =>
      layoutPort.push(HOST_A, layoutOf(split(tile('t1', [PIN_1]), tile('t-new', [PIN_2])), '2')),
    );
    await act(async () => {});
    expect(screen.getAllByTestId('terminal-panel')).toHaveLength(2);
    await waitFor(() => expect(spaceWrites(clientStatePort).at(-1)?.focusedTile).toBe('t-new'));
  });

  it('navigator_focuses_an_existing_pinned_tab', async () => {
    const { layoutPort, clientStatePort } = await mountApp({
      layout: layoutOf(split(tile('t1', [PIN_1]), tile('t2', [PIN_3, PIN_2], PIN_3))),
    });
    await clickTerminal('term-2');
    expect(mutations(layoutPort)).toEqual([]);
    const last = spaceWrites(clientStatePort).at(-1);
    expect(last?.focusedTile).toBe('t2');
    expect(last?.activeTabs['t2']).toBe(PIN_2);
    expect(last?.activeTerminal).toBe('term-2');
    expect(lastWrites(clientStatePort)).toHaveLength(1);
    expect(within(bodyOf('t2')).getByTestId('terminal-panel').textContent).toContain('deploy');
  });

  it('navigator_opens_no_tab_while_held', async () => {
    const { hostsPort, layoutPort, clientStatePort } = await mountApp({ layout: layoutOf(tile('t1', [PIN_1])) });
    act(() => hostsPort.setConnection('mac', { kind: 'reconnecting', attempt: 1 }));
    act(() => layoutPort.emit({ kind: 'disconnected', hostId: HOST_A }));
    await act(async () => {});
    await clickTerminal('term-2');
    expect(mutations(layoutPort)).toEqual([]);
    expect(spaceWrites(clientStatePort).at(-1)?.activeTerminal).toBe('term-2');
    expect(lastWrites(clientStatePort)).toHaveLength(1);
  });

  it('navigator_opens_no_tab_when_the_host_names_no_backend_instance', async () => {
    const { layoutPort, clientStatePort } = await mountApp({
      hosts: hostsOf({ id: 'herdr-core', instance: null }),
      layout: layoutOf(tile('t1', [])),
    });
    await clickTerminal('term-2');
    expect(mutations(layoutPort)).toEqual([]);
    expect(spaceWrites(clientStatePort).at(-1)?.activeTerminal).toBe('term-2');
  });

  describe('the palette', () => {
    async function openPalette(entry: string, text = '') {
      fireEvent.keyDown(window, { key: 'k', metaKey: true });
      const list = await screen.findByRole('listbox');
      const option = within(list)
        .getAllByRole('option')
        .find((o) => o.getAttribute('data-palette-entry') === entry && (o.textContent ?? '').includes(text));
      if (option === undefined) throw new Error(`no ${entry} entry`);
      fireEvent.click(option);
      await act(async () => {});
    }

    async function withPanels(layout: LayoutSnapshot, state = stored()) {
      const panelPort = new MemoryPanelPort();
      const rig = await mountApp({ layout, panelPort, state });
      await act(async () => {
        panelPort.emit({ kind: 'list', hostId: HOST_A, request: null, list: { space: PATH, panels: [], skipped: [], error: null } });
      });
      return rig;
    }

    it('show_the_active_terminal_here_adds_the_tab_when_no_tile_holds_it', async () => {
      const { layoutPort, clientStatePort } = await withPanels(layoutOf(tile('t1', ['p1'])));
      await openPalette('show-active-terminal');
      expect(mutations(layoutPort)).toEqual([[{ op: 'add_panel', panel: ACTIVE_TERMINAL_TAB, tile: 't1' }]]);
      const last = spaceWrites(clientStatePort).at(-1);
      expect(last?.activeTabs['t1']).toBe(ACTIVE_TERMINAL_TAB);
      expect(last?.focusedTile).toBe('t1');
    });

    it('show_the_active_terminal_here_moves_the_tab_to_the_focused_tile', async () => {
      const { layoutPort } = await withPanels(
        layoutOf(split(tile('t1', ['p1', 'p2']), tile('t2', [ACTIVE_TERMINAL_TAB]))),
        stored({ focusedTile: 't1' }),
      );
      await openPalette('show-active-terminal');
      expect(mutations(layoutPort)).toEqual([
        [{ op: 'move_panel', panel: ACTIVE_TERMINAL_TAB, toTile: 't1', index: 2 }],
      ]);
    });

    it('show_the_active_terminal_here_only_selects_a_tab_the_focused_tile_already_holds', async () => {
      const { layoutPort, clientStatePort } = await withPanels(
        layoutOf(tile('t1', ['p1', ACTIVE_TERMINAL_TAB], 'p1')),
      );
      await openPalette('show-active-terminal');
      expect(mutations(layoutPort)).toEqual([]);
      const last = spaceWrites(clientStatePort).at(-1);
      expect(last?.activeTabs['t1']).toBe(ACTIVE_TERMINAL_TAB);
      expect(bodyOf('t1').querySelector('[data-testid="terminal-panel"]')).not.toBeNull();
    });

    it('is not offered where the focused tile already shows the follow-active tab', async () => {
      await withPanels(layoutOf(tile('t1', [ACTIVE_TERMINAL_TAB])));
      fireEvent.keyDown(window, { key: 'k', metaKey: true });
      const list = await screen.findByRole('listbox');
      expect(within(list).queryAllByRole('option').some((o) => o.getAttribute('data-palette-entry') === 'show-active-terminal')).toBe(false);
    });

    it('the palette focus-terminal opens the terminal beside a pinned tab', async () => {
      const { layoutPort } = await withPanels(layoutOf(tile('t1', [PIN_1])));
      await openPalette('focus-terminal', 'deploy');
      expect(mutations(layoutPort)).toEqual([
        [
          { op: 'split_tile', tile: 't1', dir: 'h', newTile: 't-new', ratio: 0.5, panel: null },
          { op: 'add_panel', panel: PIN_2, tile: 't-new' },
        ],
      ]);
    });
  });
});
