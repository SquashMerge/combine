import { act, render, waitFor } from '@testing-library/react';
import type { ReactElement } from 'react';
import { describe, expect, it } from 'vitest';
import type { DecimalU64, HostId } from '../../src/model/ids';
import { type PanelContent, PanelFetchError, type PanelEntry, type PanelState } from '../../src/model/panels';
import type { RenderOutcome } from '../../src/model/render';
import { INERT_OPEN_URL, ViewPanel, type ViewPanelProps } from '../../src/panels/ViewPanel';
import { MemoryPanelPort } from '../support/memoryPanelPort';


export const HOST = 'host-1' as HostId;

export function listed(revision: string, state: PanelState = 'validated', error: PanelContent['error'] = null): PanelEntry {
  return {
    kind: 'listed',
    summary: {
      id: 'p',
      title: null,
      renderer: 'markdown',
      revision: revision as DecimalU64,
      state,
      error,
      createdBy: null,
    },
  };
}

export function content(revision: string, source = `# ${revision}`): PanelContent {
  return {
    space: '/work',
    id: 'p',
    revision: revision as DecimalU64,
    renderer: 'markdown',
    source,
    state: 'validated',
    error: null,
  };
}

export function textFragment(text: string): RenderOutcome {
  const fragment = document.createDocumentFragment();
  const p = document.createElement('p');
  p.textContent = text;
  fragment.append(p);
  return { ok: true, fragment };
}

/** A renderer double whose results the test releases itself. */
export class ManualRender {
  readonly calls: { source: string; settle: (outcome: RenderOutcome) => void }[] = [];

  readonly render = (_renderer: string, source: string): Promise<RenderOutcome> =>
    new Promise((resolve) => {
      this.calls.push({ source, settle: resolve });
    });
}

/** Renders the source as one paragraph, at once. */
export const instantRender = (_renderer: string, source: string): Promise<RenderOutcome> =>
  Promise.resolve(textFragment(source));

export function mountPanel(overrides: Partial<ViewPanelProps> = {}) {
  const port = overrides.port ?? new MemoryPanelPort();
  let props: ViewPanelProps = {
    hostId: HOST,
    space: '/work',
    panelId: 'p',
    focused: false,
    entry: { kind: 'loading' },
    session: 1,
    connected: true,
    openUrlPort: INERT_OPEN_URL,
    render: instantRender,
    ...overrides,
    port,
  };
  const element = (): ReactElement => <ViewPanel {...props} />;
  const view = render(element());
  const section = () => view.container.querySelector('section.view-panel') as HTMLElement;
  return {
    port: port as MemoryPanelPort,
    view,
    section,
    content: () => section().querySelector('.view-panel-content') as HTMLElement,
    viewState: () => section().getAttribute('data-view-state'),
    update(next: Partial<ViewPanelProps>) {
      props = { ...props, ...next };
      view.rerender(element());
    },
  };
}

/** Lets promise chains (fetch, render, report) settle inside act. */
export const flush = () => act(async () => { await new Promise((r) => setTimeout(r, 0)); });

function answered(answers: Record<string, ReturnType<typeof content>>) {
  const port = new MemoryPanelPort();
  port.autoAnswer((panel) => answers[panel]);
  return port;
}

describe('ViewPanel', () => {
  it('fetches_the_listed_revision_and_shows_it', async () => {
    const port = answered({ p: content('5', 'hello') });
    const panel = mountPanel({ port, entry: listed('5'), focused: true });
    expect(panel.viewState()).toBe('fetching');
    await waitFor(() => expect(panel.viewState()).toBe('rendered'));
    expect(port.sends.map((s) => [s.hostId, s.space, s.panel])).toEqual([[HOST, '/work', 'p']]);
    expect(panel.content().textContent).toBe('hello');
    expect(panel.section().getAttribute('data-panel-id')).toBe('p');
    expect(panel.section().getAttribute('data-shown-revision')).toBe('5');
    expect(panel.section().querySelector('[role="status"]')).toBeNull();
  });

  it('shows Loading… before the first list', () => {
    const panel = mountPanel();
    expect(panel.viewState()).toBe('loading');
    expect(panel.section().querySelector('[role="status"]')?.textContent).toBe('Loading…');
  });

  it('reports_each_shown_revision_once', async () => {
    const port = answered({ p: content('5') });
    const panel = mountPanel({ port, entry: listed('5') });
    await waitFor(() => expect(port.reports).toHaveLength(1));
    expect(port.reports[0]).toEqual({
      hostId: HOST,
      report: { space: '/work', panel: 'p', revision: '5', status: 'rendered', error: null },
    });
    panel.update({ entry: listed('5') });
    panel.update({ focused: true });
    await flush();
    expect(port.reports).toHaveLength(1);
  });

  it('never_reports_a_revision_it_did_not_show', async () => {
    const port = new MemoryPanelPort();
    const manual = new ManualRender();
    const panel = mountPanel({ port, entry: listed('5'), render: manual.render });
    await flush();
    port.sends[0]?.resolve(content('5'));
    await flush();
    expect(manual.calls).toHaveLength(1);
    panel.update({ entry: listed('6') });
    await flush();
    port.sends[1]?.resolve(content('6'));
    await flush();
    expect(manual.calls).toHaveLength(2);
    // The older render finishes after the newer reply took over: it is dropped, never reported.
    await act(async () => manual.calls[0]?.settle(textFragment('five')));
    expect(panel.content().textContent).toBe('');
    expect(port.reports).toEqual([]);
    await act(async () => manual.calls[1]?.settle(textFragment('six')));
    expect(panel.content().textContent).toBe('six');
    expect(port.reports.map((r) => r.report.revision)).toEqual(['6']);
  });

  it('reports_render_failed_with_render_error', async () => {
    const port = answered({ p: content('5') });
    const panel = mountPanel({
      port,
      entry: listed('5'),
      render: async () => ({ ok: false, error: { message: 'bad source', line: 3, column: 2 } }),
    });
    await waitFor(() => expect(port.reports).toHaveLength(1));
    expect(port.reports[0]?.report).toEqual({
      space: '/work',
      panel: 'p',
      revision: '5',
      status: 'render_failed',
      error: { code: 'render_error', message: 'bad source', line: 3, column: 2, snippet: null },
    });
    expect(panel.viewState()).toBe('render_failed');
    expect(panel.section().querySelector('[role="alert"]')?.textContent).toBe(
      "Couldn't draw this panel at line 3, column 2: bad source",
    );
    expect(panel.content().childNodes).toHaveLength(0);
  });

  it('validating_and_failed_revisions_are_not_reported', async () => {
    const port = new MemoryPanelPort();
    const panel = mountPanel({ port, entry: listed('5', 'validating') });
    panel.update({
      entry: listed('6', 'failed', { code: 'syntax', message: 'unexpected token', line: 2, column: 11, snippet: '  A -> B' }),
    });
    await flush();
    expect(port.sends).toEqual([]);
    expect(port.reports).toEqual([]);
    expect(panel.viewState()).toBe('failed');
    const alert = panel.section().querySelector('[role="alert"]');
    expect(alert?.textContent).toContain('Validation failed at line 2, column 11: unexpected token');
    expect(alert?.querySelector('pre.view-panel-snippet')?.textContent).toBe('  A -> B');
  });

  it('shows the fetch failure', async () => {
    const port = new MemoryPanelPort();
    const panel = mountPanel({ port, entry: listed('5') });
    await flush();
    await act(async () => port.sends[0]?.reject(new PanelFetchError('unknown_panel', 'gone')));
    expect(panel.viewState()).toBe('fetch_failed');
    expect(panel.section().querySelector('[role="alert"]')?.textContent).toBe("Couldn't load panel p: gone");
  });

  it('clears the content and shows missing when the panel is unlisted', async () => {
    const port = answered({ p: content('5', 'hello') });
    const panel = mountPanel({ port, entry: listed('5') });
    await waitFor(() => expect(panel.viewState()).toBe('rendered'));
    panel.update({ entry: { kind: 'missing' } });
    expect(panel.viewState()).toBe('missing');
    expect(panel.content().childNodes).toHaveLength(0);
    expect(panel.section().querySelector('[role="status"]')?.textContent).toBe('Panel p is missing');
    expect(panel.section().hasAttribute('data-shown-revision')).toBe(false);
  });

  it('a_render_finishing_while_disconnected_is_reported_exactly_once_after_reconnect', async () => {
    const port = new MemoryPanelPort();
    const manual = new ManualRender();
    const panel = mountPanel({ port, entry: listed('5'), render: manual.render });
    await flush();
    port.sends[0]?.resolve(content('5'));
    await flush();
    panel.update({ connected: false, session: 2 });
    await act(async () => manual.calls[0]?.settle(textFragment('five')));
    expect(panel.content().textContent).toBe('five');
    expect(port.reports).toEqual([]);
    panel.update({ connected: true, entry: listed('5') });
    await flush();
    expect(port.reports).toHaveLength(1);
    panel.update({ entry: listed('5') });
    await flush();
    expect(port.reports).toHaveLength(1);
  });

  it('the_shown_revision_is_reported_again_after_a_reconnect', async () => {
    const port = answered({ p: content('5') });
    const panel = mountPanel({ port, entry: listed('5') });
    await waitFor(() => expect(port.reports).toHaveLength(1));
    panel.update({ connected: false, session: 2 });
    await flush();
    expect(port.reports).toHaveLength(1);
    panel.update({ connected: true, entry: listed('5') });
    await waitFor(() => expect(port.reports).toHaveLength(2));
    expect(port.reports[1]?.report.revision).toBe('5');
    await flush();
    expect(port.reports).toHaveLength(2);
    // The shown revision is not fetched again.
    expect(port.sends).toHaveLength(1);
  });

  it('a_fetch_straddling_a_disconnect_and_reconnect_is_sent_again_and_reported_once', async () => {
    const port = new MemoryPanelPort();
    const panel = mountPanel({ port, entry: listed('5') });
    await flush();
    expect(port.sends).toHaveLength(1);
    panel.update({ connected: false, session: 2 });
    // The rejection lands after the bumped session was synced.
    await act(async () => port.sends[0]?.reject(new PanelFetchError(null, 'The host disconnected.')));
    panel.update({ connected: true, entry: listed('5') });
    await flush();
    expect(port.sends).toHaveLength(2);
    await act(async () => port.sends[1]?.resolve(content('5', 'five')));
    await waitFor(() => expect(panel.viewState()).toBe('rendered'));
    await flush();
    expect(panel.content().textContent).toBe('five');
    expect(port.reports.map((r) => r.report.revision)).toEqual(['5']);
  });

  it('an_out_of_order_list_never_replaces_a_newer_revision', async () => {
    const port = answered({ p: content('9', 'nine') });
    const panel = mountPanel({ port, entry: listed('9') });
    await waitFor(() => expect(panel.viewState()).toBe('rendered'));
    panel.update({ entry: listed('7') });
    await flush();
    expect(port.sends).toHaveLength(1);
    expect(panel.content().textContent).toBe('nine');
    expect(panel.section().getAttribute('data-shown-revision')).toBe('9');
  });

  it('a_late_reply_is_dropped', async () => {
    const port = new MemoryPanelPort();
    const panel = mountPanel({ port, entry: listed('5') });
    await flush();
    panel.update({ entry: listed('6') });
    await flush();
    expect(port.sends).toHaveLength(2);
    await act(async () => port.sends[1]?.resolve(content('6', 'six')));
    await waitFor(() => expect(panel.content().textContent).toBe('six'));
    await act(async () => port.sends[0]?.resolve(content('5', 'five')));
    await flush();
    expect(panel.content().textContent).toBe('six');
    expect(panel.section().getAttribute('data-shown-revision')).toBe('6');
    expect(port.reports.map((r) => r.report.revision)).toEqual(['6']);
  });

  it('an_update_during_a_fetch_fetches_only_the_newer_revision', async () => {
    const port = new MemoryPanelPort(1);
    port.autoAnswer((panel) => (panel === 'p' ? content('6', 'six') : content('0', 'busy')));
    const panel = mountPanel({ port, entry: { kind: 'loading' } });
    // The queue's one slot is held by another panel, so p's first request waits.
    void port.fetchPanel(HOST, '/work', 'other', false);
    panel.update({ entry: listed('5') });
    panel.update({ entry: listed('6') });
    await waitFor(() => expect(panel.viewState()).toBe('rendered'));
    expect(port.sends.filter((s) => s.panel === 'p')).toHaveLength(1);
    expect(panel.content().textContent).toBe('six');
    expect(port.reports.map((r) => r.report.revision)).toEqual(['6']);
  });

  it('a_render_overtaken_by_a_newer_reply_is_never_shown', async () => {
    const port = new MemoryPanelPort();
    const manual = new ManualRender();
    const panel = mountPanel({ port, entry: listed('5'), render: manual.render });
    await flush();
    port.sends[0]?.resolve(content('5'));
    await flush();
    panel.update({ entry: listed('6') });
    await flush();
    port.sends[1]?.resolve(content('6'));
    await flush();
    await act(async () => manual.calls[1]?.settle(textFragment('six')));
    await act(async () => manual.calls[0]?.settle(textFragment('five')));
    expect(panel.content().textContent).toBe('six');
    expect(panel.section().getAttribute('data-shown-revision')).toBe('6');
  });

  it('sends nothing after unmount', async () => {
    const port = new MemoryPanelPort();
    const manual = new ManualRender();
    const panel = mountPanel({ port, entry: listed('5'), render: manual.render });
    await flush();
    port.sends[0]?.resolve(content('5'));
    await flush();
    panel.view.unmount();
    await act(async () => manual.calls[0]?.settle(textFragment('five')));
    expect(port.reports).toEqual([]);
  });

  it('a_failed_refresh_is_shown_beside_the_older_fragment_until_a_newer_revision_renders', async () => {
    const port = new MemoryPanelPort();
    const panel = mountPanel({ port, entry: listed('1') });
    await flush();
    port.sends[0]?.resolve(content('1', 'one'));
    await waitFor(() => expect(panel.viewState()).toBe('rendered'));

    panel.update({ entry: listed('2') });
    await flush();
    port.sends[1]?.reject(new PanelFetchError(null, 'No reply'));
    await waitFor(() => expect(panel.viewState()).toBe('fetch_failed'));
    expect(panel.section().querySelector('[role="alert"]')?.textContent).toBe("Couldn't load panel p: No reply");
    expect(panel.content().textContent).toBe('one');
    expect(panel.section().getAttribute('data-shown-revision')).toBe('1');

    panel.update({ entry: listed('3') });
    await flush();
    port.sends[2]?.resolve(content('3', 'three'));
    await waitFor(() => expect(panel.viewState()).toBe('rendered'));
    expect(panel.section().querySelector('[role="alert"]')).toBeNull();
    expect(panel.content().textContent).toBe('three');
  });
});
