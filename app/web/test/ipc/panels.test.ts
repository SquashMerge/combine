import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createTauriPanelPort } from '../../src/ipc/panels';
import type { DecimalU64, HostId } from '../../src/model/ids';
import type { PanelsEvent } from '../../src/model/panels';
import { loadPanelsEventFixture } from '../support/memoryPanelPort';

const { invoke, listen } = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen }));

const fixture = loadPanelsEventFixture();
const host = 'host-1' as HostId;

type Handler = (event: { payload: unknown }) => void;
let handler: Handler | null;
let order: string[];

beforeEach(() => {
  invoke.mockReset();
  listen.mockReset();
  handler = null;
  order = [];
  listen.mockImplementation(async (name: string, h: Handler) => {
    order.push(`listen:${name}`);
    handler = h;
    await new Promise((r) => setTimeout(r, 5));
    order.push('listen-resolved');
    return () => {};
  });
});

function emit(payload: unknown): void {
  if (handler === null) throw new Error('no listener');
  handler({ payload });
}

const panelReply = (request: string, id = 'notes') => ({
  ...fixture.panel,
  request,
  panel: { ...(fixture.panel['panel'] as object), id },
});

describe('createTauriPanelPort', () => {
  it('delivers lists and disconnects to listeners, and no replies', async () => {
    const port = createTauriPanelPort();
    const seen: PanelsEvent[] = [];
    await port.onPanelsEvent((event) => seen.push(event));
    emit(fixture.list);
    emit(fixture.panel);
    emit(fixture.error);
    emit(fixture.disconnected);
    expect(seen.map((event) => event.kind)).toEqual(['list', 'disconnected']);
    expect(order).toEqual(['listen:panels-changed', 'listen-resolved']);
  });

  it('stops delivering after unsubscribe', async () => {
    const port = createTauriPanelPort();
    const seen: PanelsEvent[] = [];
    const stop = await port.onPanelsEvent((event) => seen.push(event));
    stop();
    emit(fixture.push);
    expect(seen).toEqual([]);
  });

  it('resolves a fetch with the reply for its request, registering the listener first', async () => {
    invoke.mockImplementation(async (cmd: string) => {
      order.push(`invoke:${cmd}`);
      queueMicrotask(() => emit(panelReply('9')));
      return '9';
    });
    const port = createTauriPanelPort();
    const content = await port.fetchPanel(host, '/work', 'notes', false);
    expect(content).toMatchObject({ id: 'notes', revision: '12', source: '# Notes\n' });
    expect(order).toEqual(['listen:panels-changed', 'listen-resolved', 'invoke:get_panel']);
    expect(invoke).toHaveBeenCalledWith('get_panel', { hostId: host, space: '/work', panel: 'notes' });
  });

  it('rejects a fetch with the host error and with an invoke failure', async () => {
    invoke.mockImplementationOnce(async () => {
      queueMicrotask(() => emit(fixture.error));
      return '10';
    });
    const port = createTauriPanelPort();
    await expect(port.fetchPanel(host, '/work', 'gone', false)).rejects.toMatchObject({
      code: 'unknown_panel',
      message: 'the space has no panel gone',
    });
    invoke.mockRejectedValueOnce('not connected');
    await expect(port.fetchPanel(host, '/work', 'x', false)).rejects.toMatchObject({
      code: null,
      message: 'not connected',
    });
  });

  it('a disconnect rejects the fetches in flight', async () => {
    invoke.mockResolvedValue('11');
    const port = createTauriPanelPort();
    const fetch = port.fetchPanel(host, '/work', 'notes', false);
    await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
    await Promise.resolve();
    emit(fixture.disconnected);
    await expect(fetch).rejects.toMatchObject({ message: 'The host disconnected.' });
  });

  it('caps_in_flight_get_panel_at_four', async () => {
    let next = 100;
    invoke.mockImplementation(async () => String(next++));
    const port = createTauriPanelPort();
    const fetches = Array.from({ length: 9 }, (_, n) => port.fetchPanel(host, '/work', `p${n}`, false));
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(4));
    await new Promise((r) => setTimeout(r, 10));
    expect(invoke).toHaveBeenCalledTimes(4);
    for (let n = 0; n < 9; n += 1) {
      const request = String(100 + n);
      await vi.waitFor(() => expect(invoke.mock.calls.length).toBeGreaterThan(n));
      await new Promise((r) => setTimeout(r, 0));
      emit(panelReply(request, `p${n}`));
      expect(invoke.mock.calls.length).toBeLessThanOrEqual(Math.min(9, n + 1 + 4));
    }
    await Promise.all(fetches);
    expect(invoke).toHaveBeenCalledTimes(9);
  });

  it('reportRender sends the report and resolves true', async () => {
    invoke.mockResolvedValue(undefined);
    const port = createTauriPanelPort();
    const report = {
      space: '/work',
      panel: 'notes',
      revision: '12' as DecimalU64,
      status: 'rendered' as const,
      error: null,
    };
    await expect(port.reportRender(host, report)).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith('report_render', { hostId: host, report });
  });

  it('reportRender resolves false on a rejection', async () => {
    invoke.mockRejectedValue('not connected');
    const port = createTauriPanelPort();
    await expect(
      port.reportRender(host, { space: '/w', panel: 'p', revision: '1' as DecimalU64, status: 'rendered', error: null }),
    ).resolves.toBe(false);
  });
});
