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
      useSpaceLayout(port, props.hosts, props.selected),
    { initialProps: { hosts: initialHosts, selected: initial } },
  );
}

describe('useSpaceLayout', () => {
  it("switching to another host's space unwatches the previous host", async () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    view.rerender({ hosts: hosts(), selected: key(HOST_B, '/src/three') });
    await waitFor(() => expect(port.calls).toHaveLength(3));
    expect(port.calls).toEqual([
      { kind: 'watch', hostId: HOST_A, space: '/src/one' },
      { kind: 'unwatch', hostId: HOST_A },
      { kind: 'watch', hostId: HOST_B, space: '/src/three' },
    ]);
  });

  it('same-host switch sends only watch_layout', () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    view.rerender({ hosts: hosts(), selected: key(HOST_A, '/src/two') });
    expect(port.calls).toEqual([
      { kind: 'watch', hostId: HOST_A, space: '/src/one' },
      { kind: 'watch', hostId: HOST_A, space: '/src/two' },
    ]);
  });

  it('clearing selection unwatches the previous host', () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    view.rerender({ hosts: hosts(), selected: null });
    expect(port.calls).toEqual([
      { kind: 'watch', hostId: HOST_A, space: '/src/one' },
      { kind: 'unwatch', hostId: HOST_A },
    ]);
  });

  it('rewatches selected space after reconnect', () => {
    const port = new MemoryLayoutPort();
    port.unwatchError = 'not connected';
    const selected = key(HOST_A, '/src/one');
    const view = mount(port, hosts(), selected);
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

  it('does not watch a space on a host that is not connected', () => {
    const port = new MemoryLayoutPort();
    mount(port, hosts('down'), key(HOST_A, '/src/one'));
    expect(port.calls).toEqual([]);
  });

  it('unwatches and unsubscribes on unmount', async () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await waitFor(() => expect(port.listenerCount()).toBe(1));
    view.unmount();
    expect(port.listenerCount()).toBe(0);
    expect(port.calls.at(-1)).toEqual({ kind: 'unwatch', hostId: HOST_A });
  });

  it('keeps snapshots for the watched space and marks it read-only', async () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await waitFor(() => expect(port.listenerCount()).toBe(1));
    act(() => port.push(HOST_A, layout('/src/two', '9')));
    expect(view.result.current.snapshot).toBeNull();
    act(() => port.push(HOST_B, layout('/src/one', '9')));
    expect(view.result.current.snapshot).toBeNull();
    act(() => port.push(HOST_A, layout('/src/one', '5', true)));
    expect(view.result.current.snapshot?.layoutRevision).toBe('5');
    expect([...view.result.current.readOnlySpaces]).toEqual([`${HOST_A}:/src/one`]);
    act(() => port.push(HOST_A, layout('/src/one', '4')));
    expect(view.result.current.snapshot?.layoutRevision).toBe('5');
    act(() => port.emit({ kind: 'disconnected', hostId: HOST_A }));
    expect(view.result.current.snapshot).toBeNull();
    expect(view.result.current.readOnlySpaces.size).toBe(0);
  });

  it('sends a split through the queue against the held revision', async () => {
    const port = new MemoryLayoutPort();
    const view = mount(port, hosts(), key(HOST_A, '/src/one'));
    await waitFor(() => expect(port.listenerCount()).toBe(1));
    act(() => port.push(HOST_A, layout('/src/one', '5')));
    act(() => view.result.current.split('t1', 'h'));
    await waitFor(() => expect(port.calls.at(-1)?.kind).toBe('mutate'));
    const call = port.calls.at(-1);
    expect(call?.kind === 'mutate' && call.baseLayoutRevision).toBe('5');
    expect(call?.kind === 'mutate' && call.ops[0]?.op).toBe('split_tile');
  });
});
