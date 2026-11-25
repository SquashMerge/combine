import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { type ClientState, defaultClientState } from '../../src/model/clientState';
import type { DecimalU64, HostId } from '../../src/model/ids';
import type { LayoutNode, LayoutSnapshot } from '../../src/model/layout';
import { type PanelContent, PanelFetchError, type PanelSummary, parsePanelsChanged } from '../../src/model/panels';
import { App } from '../../src/shell/App';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryLayoutPort } from '../support/memoryLayoutPort';
import { MemoryPanelPort, loadPanelsEventFixture } from '../support/memoryPanelPort';
import { MemoryTerminalPort } from '../support/memoryTerminalPort';
import { HOST_A, host, snapshot, space } from '../support/snapshots';

const PATH = '/src/alpha';
const connected = { kind: 'connected' } as const;

function saved(): ClientState {
  return {
    ...defaultClientState(),
    lastSpace: { spaceId: PATH, selectedAtMs: '5' as DecimalU64 },
    spaces: { [PATH]: { spaceId: PATH, focusedTile: null, activeTerminal: null, activeTabs: {}, groupState: {} } },
  };
}

function summary(id: string, title: string | null = null): PanelSummary {
  return { id, title, renderer: 'markdown', revision: '1' as DecimalU64, state: 'validated', error: null, createdBy: null, lifetime: null };
}

function content(id: string): PanelContent {
  return {
    space: PATH,
    id,
    revision: '1' as DecimalU64,
    renderer: 'markdown',
    source: `# ${id}`,
    state: 'validated',
    error: null,
  };
}

function tiles(count: number): LayoutNode {
  const tile = (n: number): LayoutNode => ({ type: 'tile', id: `t${n}`, panels: [`p${n}`], active: `p${n}` });
  let node = tile(count - 1);
  for (let n = count - 2; n >= 0; n -= 1) node = { type: 'split', dir: 'h', ratio: 0.5, a: tile(n), b: node };
  return node;
}

function layoutOf(root: LayoutNode): LayoutSnapshot {
  return { space: PATH, layoutRevision: '1' as DecimalU64, root, readOnly: false, error: null, dropped: null };
}

async function mount(panelPort: MemoryPanelPort) {
  const layoutPort = new MemoryLayoutPort();
  render(
    <App
      terminalPort={new MemoryTerminalPort()}
      port={new MemoryHostsPort(snapshot([host('mac', HOST_A, connected, [space(HOST_A, PATH)])]))}
      clientStatePort={new MemoryClientStatePort(new Map<HostId, ClientState>([[HOST_A, saved()]]))}
      layoutPort={layoutPort}
      panelPort={panelPort}
      newTileId={() => 't-new'}
    />,
  );
  await waitFor(() => expect(layoutPort.calls).toHaveLength(1));
  await waitFor(() => expect(panelPort.listenerCount()).toBe(1));
  return layoutPort;
}

const states = () =>
  [...document.querySelectorAll('section.view-panel')].map((node) => node.getAttribute('data-view-state'));

describe('App with panels', () => {
  it('a_listed_panel_is_fetched_and_shown_in_its_tile', async () => {
    const fixture = loadPanelsEventFixture();
    const panelPort = new MemoryPanelPort();
    const reply = parsePanelsChanged(fixture.panel);
    if (reply.kind !== 'panel') throw new Error('a panel reply');
    panelPort.autoAnswer({ notes: { ...reply.panel, space: PATH } });
    const layoutPort = await mount(panelPort);
    act(() =>
      layoutPort.push(
        HOST_A,
        layoutOf({ type: 'tile', id: 't1', panels: ['notes', 'classes'], active: 'notes' }),
      ),
    );
    const list = parsePanelsChanged(fixture.list);
    if (list.kind !== 'list') throw new Error('a list');
    act(() => panelPort.emit({ ...list, hostId: HOST_A, list: { ...list.list, space: PATH } }));
    await waitFor(() =>
      expect(document.querySelector('[data-tile-id="t1"] section.view-panel')?.getAttribute('data-view-state')).toBe('rendered'),
    );
    const tile = document.querySelector('[data-tile-id="t1"]') as HTMLElement;
    expect(tile.querySelector('h1')?.textContent).toBe('Notes');
    expect(tile.querySelector('[role="tab"][data-panel-id="classes"]')?.textContent).toBe('Class diagram');
    expect(tile.querySelector('[role="tab"][data-panel-id="notes"]')?.textContent).toBe('notes');
    expect(panelPort.sends.map((s) => [s.hostId, s.space, s.panel])).toEqual([[HOST_A, PATH, 'notes']]);
    expect(panelPort.reports).toHaveLength(1);
    expect(panelPort.reports[0]?.report).toMatchObject({ panel: 'notes', revision: '12', status: 'rendered' });
  });

  it('twenty_four_panel_tiles_are_all_fetched_without_tripping_the_daemon_limit', async () => {
    const panelPort = new MemoryPanelPort();
    expect(panelPort.daemonLimit).toBe(16);
    panelPort.autoAnswer((panel) => content(panel));
    const layoutPort = await mount(panelPort);
    act(() => layoutPort.push(HOST_A, layoutOf(tiles(24))));
    const ids = Array.from({ length: 24 }, (_, n) => `p${n}`);
    act(() =>
      panelPort.emit({
        kind: 'list',
        hostId: HOST_A,
        request: null,
        list: { space: PATH, panels: ids.map((id) => summary(id)), skipped: [], error: null },
      }),
    );
    await waitFor(() => expect(states()).toEqual(Array(24).fill('rendered')), { timeout: 10_000 });
    expect(panelPort.sends).toHaveLength(24);
    expect(panelPort.maxOutstanding).toBeLessThanOrEqual(4);
    expect(panelPort.tripped).toBe(false);
  });

  it('keeps the shown panel through a disconnect and reports again after the reconnect', async () => {
    const panelPort = new MemoryPanelPort();
    panelPort.autoAnswer((panel) => content(panel));
    const layoutPort = await mount(panelPort);
    act(() => layoutPort.push(HOST_A, layoutOf(tiles(1))));
    const list = {
      kind: 'list' as const,
      hostId: HOST_A,
      request: null,
      list: { space: PATH, panels: [summary('p0')], skipped: [], error: null },
    };
    act(() => panelPort.emit(list));
    await waitFor(() => expect(panelPort.reports).toHaveLength(1));
    act(() => panelPort.emit({ kind: 'disconnected', hostId: HOST_A }));
    expect(states()).toEqual(['rendered']);
    act(() => panelPort.emit(list));
    await waitFor(() => expect(panelPort.reports).toHaveLength(2));
    expect(panelPort.sends).toHaveLength(1);
  });
});

describe('App panel placement and close', () => {
  const lived = (id: string, lifetime: PanelSummary['lifetime']): PanelSummary => ({ ...summary(id), lifetime });

  async function setup(root?: LayoutNode) {
    const panelPort = new MemoryPanelPort();
    panelPort.autoAnswer((panel) => content(panel));
    const layoutPort = await mount(panelPort);
    act(() =>
      layoutPort.push(
        HOST_A,
        layoutOf(
          root ?? {
            type: 'split',
            dir: 'h',
            ratio: 0.5,
            a: { type: 'tile', id: 't1', panels: ['p1', 'p2', 'ghost'], active: 'p1' },
            b: { type: 'tile', id: 't2', panels: ['p4'], active: 'p4' },
          },
        ),
      ),
    );
    act(() =>
      panelPort.emit({
        kind: 'list',
        hostId: HOST_A,
        request: null,
        list: {
          space: PATH,
          panels: [lived('p1', 'scratch'), lived('p2', 'persistent'), lived('p3', 'persistent'), lived('p4', 'persistent')],
          skipped: [],
          error: null,
        },
      }),
    );
    await waitFor(() => expect(document.querySelector('[data-panel-id="p1"]')?.getAttribute('data-panel-entry')).toBe('listed'));
    return { panelPort, layoutPort };
  }

  const mutates = (layoutPort: MemoryLayoutPort) => layoutPort.calls.filter((call) => call.kind === 'mutate');
  const button = (name: string) => screen.getByRole('button', { name });
  const openPalette = () => fireEvent.keyDown(window, { key: 'k', metaKey: true });
  const choose = (label: string | RegExp, detail?: string) => {
    const option = screen
      .getAllByRole('option')
      .find((o) => (typeof label === 'string' ? o.querySelector('.label')?.textContent === label : label.test(o.textContent ?? '')) && (detail === undefined || (o.querySelector('.detail')?.textContent ?? '').includes(detail)));
    if (option === undefined) throw new Error(`no option ${String(label)}`);
    fireEvent.click(option);
  };
  const focused = () => document.querySelector('[data-tile-id][aria-current="true"]')?.getAttribute('data-tile-id');
  const selectedTab = (tile: string) =>
    document.querySelector(`[data-tile-id="${tile}"] [role="tab"][aria-selected="true"]`)?.getAttribute('data-panel-id');

  it('closing a persistent tab sends close_panel without asking and leaves the layout to the daemon', async () => {
    const { panelPort, layoutPort } = await setup();
    fireEvent.click(button('Close panel p2'));
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(panelPort.actions.map((a) => [a.kind, a.hostId, a.space, a.panel])).toEqual([['close', HOST_A, PATH, 'p2']]);
    expect((button('Close panel p2') as HTMLButtonElement).disabled).toBe(true);
    await act(async () => panelPort.actions[0]?.resolve());
    expect(mutates(layoutPort)).toEqual([]);
    expect(document.querySelector('[data-panel-id="p2"]')).not.toBeNull();
    expect((button('Close panel p2') as HTMLButtonElement).disabled).toBe(false);
  });

  it('closing a scratch tab asks first; Cancel sends nothing', async () => {
    const { panelPort } = await setup();
    fireEvent.click(button('Close panel p1'));
    const dialog = screen.getByRole('alertdialog');
    expect(dialog.textContent).toContain('Close scratch panel?');
    expect(dialog.textContent).toContain("Closing p1 deletes its content, and it can't be restored. Pin it first to keep it in the project.");
    expect(document.activeElement).toBe(button('Cancel'));
    fireEvent.click(button('Cancel'));
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(panelPort.actions).toEqual([]);
    fireEvent.click(button('Close panel p1'));
    fireEvent.keyDown(screen.getByRole('alertdialog'), { key: 'Escape' });
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(panelPort.actions).toEqual([]);
    fireEvent.click(button('Close panel p1'));
    fireEvent.click(button('Close and delete'));
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(panelPort.actions.map((a) => [a.kind, a.panel])).toEqual([['close', 'p1']]);
  });

  it('a failed close shows its error text', async () => {
    const { panelPort } = await setup();
    fireEvent.click(button('Close panel p2'));
    await act(async () => panelPort.actions[0]?.reject(new PanelFetchError('panel_busy', 'it is busy')));
    expect((await screen.findByText("Couldn't close p2: it is busy")).getAttribute('role')).toBe('alert');
  });

  it('a failed pin shows its error text', async () => {
    const { panelPort } = await setup();
    fireEvent.click(button('Pin panel p1'));
    expect(panelPort.actions.map((a) => [a.kind, a.panel])).toEqual([['pin', 'p1']]);
    await act(async () => panelPort.actions[0]?.reject(new PanelFetchError(null, 'no room')));
    expect(await screen.findByText("Couldn't pin p1: no room")).not.toBeNull();
  });

  it("a missing tab's close sends remove_panel", async () => {
    const { panelPort, layoutPort } = await setup();
    fireEvent.click(button('Close panel ghost'));
    await waitFor(() => expect(mutates(layoutPort)).toHaveLength(1));
    expect(mutates(layoutPort)[0]).toMatchObject({ ops: [{ op: 'remove_panel', panel: 'ghost' }] });
    expect(panelPort.actions).toEqual([]);
    expect(screen.queryByRole('alertdialog')).toBeNull();
  });

  it('palette Close panel and Pin panel act on the focused tab', async () => {
    const { panelPort } = await setup();
    openPalette();
    choose('Pin panel');
    expect(panelPort.actions.map((a) => [a.kind, a.panel])).toEqual([['pin', 'p1']]);
    openPalette();
    choose('Close panel');
    expect(screen.getByRole('alertdialog').textContent).toContain('Closing p1');
    fireEvent.click(button('Close and delete'));
    expect(panelPort.actions.map((a) => [a.kind, a.panel])).toEqual([['pin', 'p1'], ['close', 'p1']]);
  });

  it('choosing a placed panel makes its tab active and focuses its tile', async () => {
    await setup();
    expect(focused()).toBe('t1');
    openPalette();
    choose(/^p4/);
    await waitFor(() => expect(focused()).toBe('t2'));
    openPalette();
    choose(/^p2/);
    await waitFor(() => expect(focused()).toBe('t1'));
    expect(selectedTab('t1')).toBe('p2');
  });

  it('choosing a panel in no tile sends add_panel with no tile and focuses where it lands', async () => {
    const { layoutPort } = await setup();
    fireEvent.click(document.querySelector('[data-tile-id="t1"] [role="tab"]') as HTMLElement);
    openPalette();
    choose(/^p3/, 'not in a tile');
    await waitFor(() => expect(mutates(layoutPort)).toHaveLength(1));
    expect(mutates(layoutPort)[0]).toMatchObject({ ops: [{ op: 'add_panel', panel: 'p3', tile: null }] });
    expect(focused()).toBe('t1');
    act(() =>
      layoutPort.push(
        HOST_A,
        {
          ...layoutOf({
            type: 'split',
            dir: 'h',
            ratio: 0.5,
            a: { type: 'tile', id: 't1', panels: ['p1', 'p2', 'ghost'], active: 'p1' },
            b: { type: 'tile', id: 't2', panels: ['p4', 'p3'], active: 'p3' },
          }),
          layoutRevision: '2' as DecimalU64,
        },
        '2' as DecimalU64,
      ),
    );
    await waitFor(() => expect(focused()).toBe('t2'));
    expect(selectedTab('t2')).toBe('p3');
  });
});
