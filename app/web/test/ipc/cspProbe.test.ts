import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  PROBE_DEADLINE_MS,
  PROBE_READY_EVENT,
  PROBE_SETTLE_MS,
  withCspProbe,
} from '../../src/ipc/cspProbe';
import type { PanelPort } from '../../src/ipc/port';
import type { DecimalU64, HostId } from '../../src/model/ids';
import type { RenderReport } from '../../src/model/panels';
import { MemoryPanelPort } from '../support/memoryPanelPort';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

const host = 'host-1' as HostId;
const report = (panel: string, status: RenderReport['status'] = 'rendered'): RenderReport => ({
  space: '/work',
  panel,
  revision: '3' as DecimalU64,
  status,
  error: status === 'rendered' ? null : { code: 'render_error', message: 'bad', line: null, column: null, snippet: null },
});

interface Probe {
  panels: number;
  violations: unknown[];
  count: number;
  notify: (() => void) | null;
}

function install(probe: Probe | null): Window {
  const win = window as unknown as { __COMBINE_CSP_PROBE__?: Probe };
  if (probe === null) delete win.__COMBINE_CSP_PROBE__;
  else win.__COMBINE_CSP_PROBE__ = probe;
  return window;
}

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
});

afterEach(() => {
  install(null);
  vi.useRealTimers();
});

describe('withCspProbe', () => {
  it('is_inert_without_the_flag', async () => {
    const port = new MemoryPanelPort();
    const ready = vi.fn();
    window.addEventListener(PROBE_READY_EVENT, ready);
    expect(withCspProbe(port, install(null))).toBe(port);
    await port.reportRender(host, report('a'));
    vi.advanceTimersByTime(PROBE_DEADLINE_MS + PROBE_SETTLE_MS);
    expect(ready).not.toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalled();
    window.removeEventListener(PROBE_READY_EVENT, ready);
  });

  it('reports_the_init_script_buffer_once_after_settling', async () => {
    const violation = { directive: 'img-src', blockedUri: 'https://example.invalid/planted.png' };
    const probe: Probe = { panels: 2, violations: [violation], count: 3, notify: null };
    const ready = vi.fn();
    window.addEventListener(PROBE_READY_EVENT, ready);
    const inner = new MemoryPanelPort();
    const port: PanelPort = withCspProbe(inner, install(probe));
    expect(ready).toHaveBeenCalledTimes(1);
    expect(probe.notify).toBeTypeOf('function');

    await expect(port.reportRender(host, report('a'))).resolves.toBe(true);
    vi.advanceTimersByTime(PROBE_SETTLE_MS * 2);
    expect(invoke).not.toHaveBeenCalled();

    await port.reportRender(host, report('b', 'render_failed'));
    vi.advanceTimersByTime(PROBE_SETTLE_MS - 1);
    probe.notify?.();
    vi.advanceTimersByTime(PROBE_SETTLE_MS - 1);
    expect(invoke).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('csp_probe_report', {
      report: {
        renders: [
          { space: '/work', panel: 'a', revision: '3', status: 'rendered', error: null },
          { space: '/work', panel: 'b', revision: '3', status: 'render_failed', error: 'bad' },
        ],
        violations: [violation],
        violationCount: 3,
        timedOut: false,
      },
    });
    expect(inner.reports).toHaveLength(2);

    vi.advanceTimersByTime(PROBE_DEADLINE_MS);
    expect(invoke).toHaveBeenCalledTimes(1);
    window.removeEventListener(PROBE_READY_EVENT, ready);
  });

  it('forwards_close_and_pin_to_the_wrapped_port', async () => {
    const inner = new MemoryPanelPort();
    const port = withCspProbe(inner, install({ panels: 1, violations: [], count: 0, notify: null }));
    const closed = port.closePanel(host, '/work', 'a');
    const pinned = port.pinPanel(host, '/work', 'b');
    expect(inner.actions.map(({ kind, hostId, space, panel }) => ({ kind, hostId, space, panel }))).toEqual([
      { kind: 'close', hostId: host, space: '/work', panel: 'a' },
      { kind: 'pin', hostId: host, space: '/work', panel: 'b' },
    ]);
    inner.actions.forEach((action) => action.resolve());
    await expect(Promise.all([closed, pinned])).resolves.toEqual([undefined, undefined]);
  });

  it('reports_timed_out_at_the_deadline', async () => {
    const probe: Probe = { panels: 2, violations: [], count: 0, notify: null };
    const port = withCspProbe(new MemoryPanelPort(), install(probe));
    await port.reportRender(host, report('only'));
    vi.advanceTimersByTime(PROBE_DEADLINE_MS - 1);
    expect(invoke).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('csp_probe_report', {
      report: {
        renders: [{ space: '/work', panel: 'only', revision: '3', status: 'rendered', error: null }],
        violations: [],
        violationCount: 0,
        timedOut: true,
      },
    });
  });
});
