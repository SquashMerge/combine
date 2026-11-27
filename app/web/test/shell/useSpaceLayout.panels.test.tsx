import { act, renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { HostsSnapshot } from '../../src/model/hosts';
import type { DecimalU64, HostId } from '../../src/model/ids';
import type { LayoutSnapshot } from '../../src/model/layout';
import { PANELS_LOADING, type PanelList, type PanelsEvent } from '../../src/model/panels';
import type { SpaceKey } from '../../src/model/space';
import { useSpaceLayout } from '../../src/shell/useSpaceLayout';
import { MemoryLayoutPort } from '../support/memoryLayoutPort';
import { MemoryPanelPort } from '../support/memoryPanelPort';
import { HOST_A, host, snapshot, space } from '../support/snapshots';

const connected = { kind: 'connected' } as const;
const dropped = { kind: 'dropped', reason: 'gone', detail: null, retryInMs: 1000 } as const;
const ONE: SpaceKey = { hostId: HOST_A, path: '/src/one' };

function hosts(up = true): HostsSnapshot {
  return snapshot([host('mac', HOST_A, up ? connected : dropped, [space(HOST_A, '/src/one'), space(HOST_A, '/src/two')])]);
}

function layout(path: string): LayoutSnapshot {
  return {
    space: path,
    layoutRevision: '1' as DecimalU64,
    root: { type: 'tile', id: 't1', panels: [], active: null },
    readOnly: false,
    error: null,
    dropped: null,
  };
}

function list(path: string, error: string | null = null, ids: string[] = ['a']): PanelList {
  return {
    space: path,
    panels: error === null ? ids.map((id) => ({ id, title: null, renderer: 'markdown', revision: '1' as DecimalU64, state: 'validated', error: null, createdBy: null, lifetime: 'persistent' })) : [],
    skipped: [],
    error,
  };
}

const listEvent = (path: string, l: PanelList = list(path), hostId: HostId = HOST_A): PanelsEvent => ({
  kind: 'list',
  hostId,
  request: null,
  list: l,
});

function mount(layoutPort: MemoryLayoutPort, panelPort: MemoryPanelPort | undefined) {
  return renderHook(
    (props: { hosts: HostsSnapshot; selected: SpaceKey | null }) =>
      useSpaceLayout(layoutPort, props.hosts, props.selected, () => 't-new', panelPort),
    { initialProps: { hosts: hosts(), selected: ONE as SpaceKey | null } },
  );
}

const settle = () => act(() => Promise.resolve());

describe('useSpaceLayout panels', () => {
  it('has no panels without a panel port', async () => {
    const view = mount(new MemoryLayoutPort(), undefined);
    await settle();
    expect(view.result.current.panels).toBeNull();
  });

  it('no_watch_before_the_panels_listener', async () => {
    const layoutPort = new MemoryLayoutPort();
    const panelPort = new MemoryPanelPort();
    let open = (): void => {};
    panelPort.listenGate = new Promise<void>((resolve) => {
      open = resolve;
    });
    const view = mount(layoutPort, panelPort);
    await settle();
    expect(layoutPort.listenerCount()).toBe(1);
    expect(layoutPort.calls).toEqual([]);
    await act(async () => open());
    expect(layoutPort.calls).toEqual([{ kind: 'watch', hostId: HOST_A, space: '/src/one' }]);
    expect(view.result.current.panels).toEqual({ session: 0, connected: false, list: null });
  });

  it('holds the watched space list and marks it connected', async () => {
    const layoutPort = new MemoryLayoutPort();
    const panelPort = new MemoryPanelPort();
    const view = mount(layoutPort, panelPort);
    await settle();
    act(() => layoutPort.push(HOST_A, layout('/src/one')));
    act(() => panelPort.emit(listEvent('/src/one')));
    expect(view.result.current.panels).toMatchObject({ connected: true, list: { space: '/src/one' } });
  });

  it('a_loading_list_counts_as_no_list', async () => {
    const panelPort = new MemoryPanelPort();
    const view = mount(new MemoryLayoutPort(), panelPort);
    await settle();
    act(() => panelPort.emit(listEvent('/src/one', list('/src/one', PANELS_LOADING))));
    expect(view.result.current.panels).toEqual({ session: 0, connected: true, list: null });
    act(() => panelPort.emit(listEvent('/src/one')));
    expect(view.result.current.panels?.list?.panels).toHaveLength(1);
  });

  it('other_spaces_are_ignored', async () => {
    const panelPort = new MemoryPanelPort();
    const view = mount(new MemoryLayoutPort(), panelPort);
    await settle();
    act(() => panelPort.emit(listEvent('/src/two')));
    act(() => panelPort.emit(listEvent('/src/one', list('/src/one'), 'other-host' as HostId)));
    expect(view.result.current.panels).toEqual({ session: 0, connected: false, list: null });
    act(() => panelPort.emit({ kind: 'disconnected', hostId: 'other-host' }));
    expect(view.result.current.panels?.session).toBe(0);
  });

  it('a_disconnect_bumps_the_session_and_keeps_the_list', async () => {
    const panelPort = new MemoryPanelPort();
    const view = mount(new MemoryLayoutPort(), panelPort);
    await settle();
    act(() => panelPort.emit(listEvent('/src/one')));
    act(() => panelPort.emit({ kind: 'disconnected', hostId: HOST_A }));
    expect(view.result.current.panels).toMatchObject({ session: 1, connected: false, list: { space: '/src/one' } });
    view.rerender({ hosts: hosts(false), selected: ONE });
    expect(view.result.current.panels).toMatchObject({ session: 1, connected: false, list: { space: '/src/one' } });
    view.rerender({ hosts: hosts(true), selected: ONE });
    await settle();
    expect(view.result.current.panels).toMatchObject({ connected: false, list: { space: '/src/one' } });
    act(() => panelPort.emit(listEvent('/src/one', list('/src/one', null, ['a', 'b']))));
    expect(view.result.current.panels).toMatchObject({ session: 1, connected: true });
    expect(view.result.current.panels?.list?.panels).toHaveLength(2);
  });

  it('starts empty on another space and is null with nothing selected', async () => {
    const panelPort = new MemoryPanelPort();
    const view = mount(new MemoryLayoutPort(), panelPort);
    await settle();
    act(() => panelPort.emit(listEvent('/src/one')));
    view.rerender({ hosts: hosts(), selected: { hostId: HOST_A, path: '/src/two' } });
    await settle();
    expect(view.result.current.panels).toEqual({ session: 0, connected: false, list: null });
    view.rerender({ hosts: hosts(), selected: null });
    expect(view.result.current.panels).toBeNull();
  });
});
