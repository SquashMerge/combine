import { render, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';
import type { DecimalU64, HostId } from '../../src/model/ids';
import type { PanelEntry } from '../../src/model/panels';
import { INERT_OPEN_URL, ViewPanel, type ViewPanelProps } from '../../src/panels/ViewPanel';
import { OPEN_URL_ATTRIBUTE } from '../../src/renderers/links';
import { MERMAID_FIXTURES } from '../fixtures/renderers';
import { stubCanvas } from '../support/canvas';
import { MemoryOpenUrlPort } from '../support/memoryOpenUrlPort';
import { MemoryPanelPort } from '../support/memoryPanelPort';


const HOST = 'host-1' as HostId;

function listed(revision: string): PanelEntry {
  return {
    kind: 'listed',
    summary: {
      id: 'p',
      title: null,
      renderer: 'markdown',
      revision: revision as DecimalU64,
      state: 'validated',
      error: null,
      createdBy: null,
    },
  };
}

function content(revision: string, source: string) {
  return {
    space: '/work',
    id: 'p',
    revision: revision as DecimalU64,
    renderer: 'markdown' as const,
    source,
    state: 'validated' as const,
    error: null,
  };
}

function mountPanel(overrides: Partial<ViewPanelProps> & { port: MemoryPanelPort }) {
  const view = render(
    <ViewPanel
      hostId={HOST}
      space="/work"
      panelId="p"
      focused={false}
      entry={{ kind: 'loading' }}
      session={1}
      connected
      openUrlPort={INERT_OPEN_URL}
      {...overrides}
    />,
  );
  const section = () => view.container.querySelector('section.view-panel') as HTMLElement;
  return {
    view,
    section,
    content: () => section().querySelector('.view-panel-content') as HTMLElement,
    viewState: () => section().getAttribute('data-view-state'),
  };
}

beforeEach(() => stubCanvas());

describe('ViewPanel with the real renderers', () => {
  it('draws_a_mermaid_panel', async () => {
    const source = MERMAID_FIXTURES.find((f) => f.id === 'M-link')?.source ?? '';
    const port = new MemoryPanelPort();
    port.autoAnswer(() => ({ ...content('2', source), renderer: 'mermaid' }));
    const open = new MemoryOpenUrlPort();
    const entry = listed('2');
    if (entry.kind !== 'listed') throw new Error('listed');
    const panel = mountPanel({
      port,
      entry: { ...entry, summary: { ...entry.summary, renderer: 'mermaid' } },
      openUrlPort: open,
          });
    await waitFor(() => expect(panel.viewState()).toBe('rendered'), { timeout: 10_000 });
    expect(panel.content().querySelector('.combine-mermaid svg')).not.toBeNull();
    expect(panel.content().textContent).not.toContain('flowchart');
    const link = panel.content().querySelector(`[${OPEN_URL_ATTRIBUTE}]`);
    link?.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    expect(open.urls).toEqual(['https://example.com/a']);
    await waitFor(() => expect(port.reports).toHaveLength(1));
    expect(port.reports[0]?.report.status).toBe('rendered');
  });

  it('draws_a_markdown_panel', async () => {
    const port = new MemoryPanelPort();
    port.autoAnswer(() => content('3', '# Title\n\n- one\n- two\n\n[k](https://example.com/k)\n'));
    const open = new MemoryOpenUrlPort();
    const panel = mountPanel({ port, entry: listed('3'), openUrlPort: open });
    await waitFor(() => expect(panel.viewState()).toBe('rendered'), { timeout: 10_000 });
    const root = panel.content().querySelector('.combine-markdown');
    expect(root?.querySelector('h1')?.textContent).toBe('Title');
    expect(root?.querySelectorAll('li')).toHaveLength(2);
    expect(panel.content().textContent).not.toContain('# Title');
    const link = panel.content().querySelector('a');
    link?.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    expect(open.urls).toEqual(['https://example.com/k']);
  });

  it('detaches the link handler on unmount', async () => {
    const port = new MemoryPanelPort();
    port.autoAnswer(() => content('3', '[k](https://example.com/k)'));
    const open = new MemoryOpenUrlPort();
    const panel = mountPanel({ port, entry: listed('3'), openUrlPort: open });
    await waitFor(() => expect(panel.viewState()).toBe('rendered'), { timeout: 10_000 });
    const element = panel.content();
    const link = element.querySelector('a') as HTMLAnchorElement;
    panel.view.unmount();
    link.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    expect(open.urls).toEqual([]);
  });

  it('shows the renderer failure for a source that does not parse', async () => {
    const port = new MemoryPanelPort();
    const source = MERMAID_FIXTURES.find((f) => f.mustFail === true)?.source ?? 'not a diagram';
    port.autoAnswer(() => ({ ...content('4', source), revision: '4' as DecimalU64, renderer: 'mermaid' }));
    const panel = mountPanel({ port, entry: listed('4') });
    await waitFor(() => expect(panel.viewState()).toBe('render_failed'), { timeout: 10_000 });
    expect(panel.section().querySelector('[role="alert"]')?.textContent).toMatch(/^Couldn't draw this panel/);
  });
});
