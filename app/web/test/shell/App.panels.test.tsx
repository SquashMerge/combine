import { act, render, waitFor } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { type ClientState, defaultClientState } from '../../src/model/clientState';
import type { DecimalU64, HostId } from '../../src/model/ids';
import type { LayoutNode, LayoutSnapshot } from '../../src/model/layout';
import { type PanelContent, type PanelSummary, parsePanelsChanged } from '../../src/model/panels';
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
  return { id, title, renderer: 'markdown', revision: '1' as DecimalU64, state: 'validated', error: null, createdBy: null };
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
