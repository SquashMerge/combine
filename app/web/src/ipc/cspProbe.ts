import { invoke } from '@tauri-apps/api/core';
import type { PanelPort } from './port';
import { CSP_PROBE_REPORT_COMMAND } from './names';

export const PROBE_READY_EVENT = 'combine-csp-probe-ready';
export const PROBE_SETTLE_MS = 2_000;
export const PROBE_DEADLINE_MS = 120_000;

/** What the probe build's init script leaves on the window; nothing else sets it. */
interface ProbeBuffer {
  panels: number;
  violations: unknown[];
  count: number;
  notify: (() => void) | null;
}

interface ProbeWindow {
  __COMBINE_CSP_PROBE__?: ProbeBuffer;
}

/**
 * In the CSP probe build only, records each render report and, once the probe's panels have
 * settled (or a deadline passes), sends the buffer to the app once. Without the init script's
 * global it returns the port itself and does nothing.
 */
export function withCspProbe(port: PanelPort, win: Window = window): PanelPort {
  const probe = (win as unknown as ProbeWindow).__COMBINE_CSP_PROBE__;
  if (probe === undefined) return port;

  const renders: Record<string, unknown>[] = [];
  const panels = new Set<string>();
  let settle: ReturnType<typeof setTimeout> | null = null;
  let done = false;

  const finish = (timedOut: boolean): void => {
    if (done) return;
    done = true;
    if (settle !== null) clearTimeout(settle);
    clearTimeout(deadline);
    void invoke(CSP_PROBE_REPORT_COMMAND, {
      report: { renders, violations: probe.violations, violationCount: probe.count, timedOut },
    }).catch(() => undefined);
  };

  const restartSettle = (): void => {
    if (done || panels.size < probe.panels) return;
    if (settle !== null) clearTimeout(settle);
    settle = setTimeout(() => finish(false), PROBE_SETTLE_MS);
  };

  const deadline = setTimeout(() => finish(true), PROBE_DEADLINE_MS);
  probe.notify = restartSettle;
  win.dispatchEvent(new Event(PROBE_READY_EVENT));

  return {
    onPanelsEvent: (listener) => port.onPanelsEvent(listener),
    fetchPanel: (hostId, space, panel, urgent) => port.fetchPanel(hostId, space, panel, urgent),
    closePanel: (hostId, space, panel) => port.closePanel(hostId, space, panel),
    pinPanel: (hostId, space, panel) => port.pinPanel(hostId, space, panel),
    reportRender(hostId, report) {
      renders.push({
        space: report.space,
        panel: report.panel,
        revision: report.revision,
        status: report.status,
        error: report.error?.message ?? null,
      });
      panels.add(report.panel);
      restartSettle();
      return port.reportRender(hostId, report);
    },
  };
}
