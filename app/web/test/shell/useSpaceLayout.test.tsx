import { act, renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { HostsSnapshot } from '../../src/model/hosts';
import { type DecimalU64, type HostId } from '../../src/model/ids';
import type { LayoutSnapshot } from '../../src/model/layout';
import type { SpaceKey } from '../../src/model/space';
import { useSpaceLayout } from '../../src/shell/useSpaceLayout';
import { MemoryLayoutPort } from '../support/memoryLayoutPort';
import { HOST_A, HOST_B, host, snapshot, space } from '../support/snapshots';

const connected = { kind: 'connected' } as const;
const dropped = { kind: 'dropped', reason: 'gone', detail: null, retryInMs: 1000 } as const;

function hosts(a: 'up' | 'down' = 'up'): HostsSnapshot {
  return snapshot([
    host('mac', HOST_A, a === 'up' ? connected : dropped, [
      space(HOST_A, '/src/one'),
      space(HOST_A, '/src/two'),
    ]),
    host('mini', HOST_B, connected, [space(HOST_B, '/src/three')]),
  ]);
}

const key = (hostId: HostId, path: string): SpaceKey => ({ hostId, path });

function layout(path: string, revision = '1', readOnly = false): LayoutSnapshot {
  return {
    space: path,
    layoutRevision: revision as DecimalU64,
    root: { type: 'tile', id: 't1', panels: [], active: null },
    readOnly,
    error: null,
    dropped: null,
  };
}

function mount(port: MemoryLayoutPort, initialHosts = hosts(), initial: SpaceKey | null = null) {
  return renderHook(
    (props: { hosts: HostsSnapshot; selected: SpaceKey | null }) =>
      useSpaceLayout(port, props.hosts, props.selected, () => 't-new'),
    { initialProps: { hosts: initialHosts, selected: initial } },
  );
}

/** Lets the hook's asynchronous listener registration finish, so its first watch goes out. */
const settle = () => act(() => Promise.resolve());

describe('useSpaceLayout', () => {
  it("switching to another host's space unwatches the previous host", async () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await settle();
    view.rerender({ hosts: hosts(), selected: key(HOST_B, '/src/three') });
    await waitFor(() => expect(port.calls).toHaveLength(3));
    expect(port.calls).toEqual([
      { kind: 'watch', hostId: HOST_A, space: '/src/one' },
      { kind: 'unwatch', hostId: HOST_A },
      { kind: 'watch', hostId: HOST_B, space: '/src/three' },
    ]);
  });

  it('same-host switch sends only watch_layout', async () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await settle();
    view.rerender({ hosts: hosts(), selected: key(HOST_A, '/src/two') });
    expect(port.calls).toEqual([
      { kind: 'watch', hostId: HOST_A, space: '/src/one' },
      { kind: 'watch', hostId: HOST_A, space: '/src/two' },
    ]);
  });

  it('clearing selection unwatches the previous host', async () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await settle();
    view.rerender({ hosts: hosts(), selected: null });
    expect(port.calls).toEqual([
      { kind: 'watch', hostId: HOST_A, space: '/src/one' },
      { kind: 'unwatch', hostId: HOST_A },
    ]);
  });

  it('rewatches selected space after reconnect', async () => {
    const port = new MemoryLayoutPort();
    port.unwatchError = 'not connected';
    const selected = key(HOST_A, '/src/one');
    const view = mount(port, hosts(), selected);
    await settle();
    view.rerender({ hosts: hosts('down'), selected });
    view.rerender({ hosts: hosts('up'), selected });
    expect(port.calls).toEqual([
      { kind: 'watch', hostId: HOST_A, space: '/src/one' },
      { kind: 'unwatch', hostId: HOST_A },
      { kind: 'watch', hostId: HOST_A, space: '/src/one' },
    ]);
  });

  it('ignores a not connected error on unwatch but reports others', async () => {
    const port = new MemoryLayoutPort();
    port.unwatchError = 'not connected';
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    view.rerender({ hosts: hosts(), selected: null });
    await act(() => Promise.resolve());
    expect(view.result.current.error).toBeNull();
    port.unwatchError = 'boom';
    view.rerender({ hosts: hosts(), selected: key(HOST_A, '/src/one') });
    view.rerender({ hosts: hosts(), selected: null });
    await waitFor(() => expect(view.result.current.error).toBe('boom'));
  });

  it('does not watch a space on a host that is not connected', async () => {
    const port = new MemoryLayoutPort();
    mount(port, hosts('down'), key(HOST_A, '/src/one'));
    await settle();
    expect(port.calls).toEqual([]);
  });

  it('unwatches and unsubscribes on unmount', async () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await waitFor(() => expect(port.calls).toHaveLength(1));
    view.unmount();
    expect(port.listenerCount()).toBe(0);
    expect(port.calls.at(-1)).toEqual({ kind: 'unwatch', hostId: HOST_A });
  });

  it('keeps snapshots for the watched space and marks it read-only', async () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await waitFor(() => expect(port.calls).toHaveLength(1));
    act(() => port.push(HOST_A, layout('/src/two', '9')));
    expect(view.result.current.snapshot).toBeNull();
    act(() => port.push(HOST_B, layout('/src/one', '9')));
    expect(view.result.current.snapshot).toBeNull();
    act(() => port.push(HOST_A, layout('/src/one', '5', true)));
    expect(view.result.current.snapshot?.layoutRevision).toBe('5');
    expect([...view.result.current.readOnlySpaces]).toEqual([`${HOST_A}:/src/one`]);
    act(() => port.push(HOST_A, layout('/src/one', '4', true)));
    expect(view.result.current.snapshot?.layoutRevision).toBe('5');
    act(() => port.emit({ kind: 'disconnected', hostId: HOST_A }));
    expect(view.result.current.snapshot).toBeNull();
    expect([...view.result.current.readOnlySpaces]).toEqual([`${HOST_A}:/src/one`]);
    act(() => port.push(HOST_A, layout('/src/one', '6')));
    expect(view.result.current.readOnlySpaces.size).toBe(0);
  });

  it('sends a split through the queue against the held revision', async () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await waitFor(() => expect(port.calls).toHaveLength(1));
    act(() => port.push(HOST_A, layout('/src/one', '5')));
    act(() => view.result.current.split('t1', 'h'));
    await waitFor(() => expect(port.calls.at(-1)?.kind).toBe('mutate'));
    const call = port.calls.at(-1);
    expect(call?.kind === 'mutate' && call.baseLayoutRevision).toBe('5');
    expect(call?.kind === 'mutate' && call.ops[0]?.op).toBe('split_tile');
  });

  async function watching() {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await waitFor(() => expect(port.calls).toHaveLength(1));
    act(() => port.push(HOST_A, layout('/src/one', '5'), '1' as DecimalU64));
    const mutates = () => port.calls.filter((call) => call.kind === 'mutate');
    return { port, view, mutates };
  }

  it('sends the exact split and close ops, one at a time, released by reply snapshots', async () => {
    const { port, view, mutates } = await watching();
    act(() => {
      view.result.current.split('t1', 'v');
      view.result.current.close('t1');
    });
    await waitFor(() => expect(mutates()).toHaveLength(1));
    expect(mutates()[0]).toMatchObject({
      hostId: HOST_A,
      space: '/src/one',
      baseLayoutRevision: '5',
      ops: [{ op: 'split_tile', tile: 't1', dir: 'v', newTile: 't-new', ratio: 0.5, panel: null }],
    });
    act(() => port.push(HOST_A, layout('/src/one', '6'), '2' as DecimalU64));
    await waitFor(() => expect(mutates()).toHaveLength(2));
    expect(mutates()[1]).toMatchObject({
      baseLayoutRevision: '6',
      ops: [{ op: 'close_tile', tile: 't1' }],
    });
  });

  it('resends at the new base after stale_layout and a newer push', async () => {
    const { port, view, mutates } = await watching();
    act(() => view.result.current.close('t1'));
    await waitFor(() => expect(mutates()).toHaveLength(1));
    act(() => port.emit({ kind: 'error', hostId: HOST_A, request: '2' as DecimalU64, code: 'stale_layout', message: 'stale' }));
    expect(mutates()).toHaveLength(1);
    act(() => port.push(HOST_A, layout('/src/one', '6')));
    await waitFor(() => expect(mutates()).toHaveLength(2));
    expect(mutates()[1]).toMatchObject({ baseLayoutRevision: '6', ops: [{ op: 'close_tile', tile: 't1' }] });
  });

  it('shows an invalid_request message as the notice', async () => {
    const { port, view, mutates } = await watching();
    act(() => view.result.current.close('t1'));
    await waitFor(() => expect(mutates()).toHaveLength(1));
    act(() => port.emit({ kind: 'error', hostId: HOST_A, request: '2' as DecimalU64, code: 'invalid_request', message: 'no such tile' }));
    expect(view.result.current.notice).toBe('no such tile');
  });

  it('a disconnect drops queued intents', async () => {
    const { port, view, mutates } = await watching();
    act(() => {
      view.result.current.close('t1');
      view.result.current.close('t1');
    });
    await waitFor(() => expect(mutates()).toHaveLength(1));
    act(() => port.emit({ kind: 'disconnected', hostId: HOST_A }));
    act(() => port.push(HOST_A, layout('/src/one', '6'), '2' as DecimalU64));
    await act(() => Promise.resolve());
    expect(mutates()).toHaveLength(1);
  });

  it('reports a watch error that arrives before the watch resolves', async () => {
    const port = new MemoryLayoutPort();
    let open = () => undefined as void;
    port.watchGate = new Promise<void>((resolve) => {
      open = resolve;
    });
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await waitFor(() => expect(port.calls).toHaveLength(1));
    act(() => port.emit({ kind: 'error', hostId: HOST_A, request: '1' as DecimalU64, code: 'unknown_space', message: 'no such space' }));
    expect(view.result.current.error).toBeNull();
    await act(async () => open());
    expect(view.result.current.error).toBe('no such space');
  });

  it("does not take another request's error for the watch's before the watch resolves", async () => {
    const port = new MemoryLayoutPort();
    let open = () => undefined as void;
    port.watchGate = new Promise<void>((resolve) => {
      open = resolve;
    });
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await waitFor(() => expect(port.calls).toHaveLength(1));
    act(() => port.emit({ kind: 'error', hostId: HOST_A, request: '7' as DecimalU64, code: 'invalid_request', message: 'not the watch' }));
    await act(async () => open());
    expect(view.result.current.error).toBeNull();
  });

  function gateListener(port: MemoryLayoutPort): () => Promise<void> {
    let release = () => undefined as void;
    port.listenGate = new Promise<void>((resolve) => {
      release = resolve;
    });
    return () => act(async () => release());
  }

  it('waits for the event listener before the first watch, so an immediate reply is held', async () => {
    const port = new MemoryLayoutPort();
    const release = gateListener(port);
    port.onWatch = (hostId, space, request) => port.push(hostId, layout(space, '3'), request);
    const view = mount(port, hosts(), null);
    view.rerender({ hosts: hosts(), selected: key(HOST_A, '/src/one') });
    await settle();
    await release();
    await waitFor(() => expect(view.result.current.snapshot?.layoutRevision).toBe('3'));
    expect(port.calls).toEqual([{ kind: 'watch', hostId: HOST_A, space: '/src/one' }]);
    act(() => view.result.current.split('t1', 'h'));
    await waitFor(() => expect(port.calls.at(-1)?.kind).toBe('mutate'));
    const call = port.calls.at(-1);
    expect(call?.kind === 'mutate' && call.baseLayoutRevision).toBe('3');
  });

  it('sends only the latest selection once a pending listener registers', async () => {
    const port = new MemoryLayoutPort();
    const release = gateListener(port);
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    view.rerender({ hosts: hosts(), selected: key(HOST_A, '/src/two') });
    view.rerender({ hosts: hosts(), selected: key(HOST_B, '/src/three') });
    await settle();
    expect(port.calls).toEqual([]);
    await release();
    expect(port.calls).toEqual([{ kind: 'watch', hostId: HOST_B, space: '/src/three' }]);
  });

  it('sends no watch when selection is cleared before a pending listener registers', async () => {
    const port = new MemoryLayoutPort();
    const release = gateListener(port);
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    view.rerender({ hosts: hosts(), selected: null });
    await release();
    expect(port.calls).toEqual([]);
  });

  it('sends no watch and drops the listener when unmounted before it registers', async () => {
    const port = new MemoryLayoutPort();
    const release = gateListener(port);
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    view.unmount();
    await release();
    expect(port.calls).toEqual([]);
    expect(port.listenerCount()).toBe(0);
  });
});

describe('useSpaceLayout panel ops', () => {
  it('addPanel and removePanel send the exact ops', async () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await waitFor(() => expect(port.calls).toHaveLength(1));
    act(() => port.push(HOST_A, layout('/src/one', '5'), '1' as DecimalU64));
    act(() => view.result.current.addPanel('notes'));
    await waitFor(() => expect(port.calls.filter((c) => c.kind === 'mutate')).toHaveLength(1));
    act(() => port.push(HOST_A, layout('/src/one', '6'), '2' as DecimalU64));
    act(() => view.result.current.removePanel('notes'));
    await waitFor(() => expect(port.calls.filter((c) => c.kind === 'mutate')).toHaveLength(2));
    const ops = port.calls.flatMap((c) => (c.kind === 'mutate' ? [c.ops] : []));
    expect(ops).toEqual([
      [{ op: 'add_panel', panel: 'notes', tile: null }],
      [{ op: 'remove_panel', panel: 'notes' }],
    ]);
  });
});
