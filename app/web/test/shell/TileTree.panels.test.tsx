import { act, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { DecimalU64, HostId } from '../../src/model/ids';
import { parseTermId } from '../../src/model/ids';
import type { LayoutSnapshot } from '../../src/model/layout';
import type { PanelList, PanelState, PanelSummary, SpacePanelsView } from '../../src/model/panels';
import { TileTree, type TerminalMount } from '../../src/shell/TileTree';
import { fakeXtermFactory } from '../support/fakeXterm';
import { MemoryPanelPort } from '../support/memoryPanelPort';
import { HOST_ID, MemoryTerminalPort } from '../support/memoryTerminalPort';
import { installTerminalDom } from '../support/terminalDom';

const HOST = HOST_ID as HostId;
const SPACE = { hostId: HOST, path: '/work' };
const MOUNT: TerminalMount = {
  hostId: HOST_ID,
  termId: parseTermId('term-1'),
  label: { name: 'build', host: 'mini', state: 'running' },
};

function layout(panels: string[], active: string | null = panels[0] ?? null): LayoutSnapshot {
  return {
    space: '/work',
    layoutRevision: '1' as DecimalU64,
    root: {
      type: 'split',
      dir: 'h',
      ratio: 0.5,
      a: { type: 'tile', id: 't1', panels, active },
      b: { type: 'tile', id: 't2', panels: [], active: null },
    },
    readOnly: false,
    error: null,
    dropped: null,
  };
}

function summary(id: string, title: string | null = null, state: PanelState = 'validating'): PanelSummary {
  return { id, title, renderer: 'markdown', revision: '1' as DecimalU64, state, error: null, createdBy: null };
}

function view(panels: PanelSummary[], skipped: PanelList['skipped'] = [], listError: string | null = null): SpacePanelsView {
  return { session: 1, connected: true, list: { space: '/work', panels, skipped, error: listError } };
}

function mount(
  panelIds: string[],
  panels: SpacePanelsView | null,
  options: { active?: string | null; focused?: string; terminal?: TerminalMount | null } = {},
) {
  const port = new MemoryPanelPort();
  const element = (next: SpacePanelsView | null) => (
    <TileTree
      layout={layout(panelIds, options.active)}
      notice={null}
      error={null}
      focusedTile={options.focused ?? 't2'}
      activeTabs={{}}
      onFocus={vi.fn()}
      onSelectTab={vi.fn()}
      onSplit={vi.fn()}
      onClose={vi.fn()}
      terminalPort={new MemoryTerminalPort()}
      terminal={options.terminal ?? null}
      createAdapter={fakeXtermFactory().factory}
      space={SPACE}
      panels={next}
      panelPort={port}
    />
  );
  const rendered = render(element(panels));
  return { port, rerender: (next: SpacePanelsView | null) => rendered.rerender(element(next)) };
}

const tab = (id: string) => document.querySelector(`[role="tab"][data-panel-id="${id}"]`) as HTMLElement;
const body = (tile = 't1') => document.querySelector(`[data-tile-id="${tile}"] .tile-body`) as HTMLElement;
const section = (tile = 't1') => body(tile).querySelector('section.view-panel') as HTMLElement;

beforeEach(() => installTerminalDom());

describe('TileTree panel tabs', () => {
  it('tabs_show_titles', () => {
    mount(['a', 'b'], view([summary('a', 'Alpha doc'), summary('b', 'Beta')]));
    expect(tab('a').textContent).toBe('Alpha doc');
    expect(tab('b').textContent).toBe('Beta');
    expect(tab('a').getAttribute('data-panel-entry')).toBe('listed');
  });

  it('a_panel_without_a_title_shows_its_id', () => {
    mount(['a'], view([summary('a', null)]));
    expect(tab('a').textContent).toBe('a');
  });

  it('unlisted_ids_show_their_id', () => {
    mount(['ghost', 'broken'], view([summary('other', 'Other')], [{ panel: 'broken', reason: 'bad' }]));
    expect(tab('ghost').textContent).toBe('ghost');
    expect(tab('ghost').getAttribute('data-panel-entry')).toBe('missing');
    expect(tab('broken').textContent).toBe('broken');
    expect(tab('broken').getAttribute('data-panel-entry')).toBe('skipped');
  });

  it('before_the_first_list_tabs_show_loading', () => {
    mount(['a', 'b'], { session: 0, connected: false, list: null });
    expect(tab('a').textContent).toBe('a');
    expect(tab('a').getAttribute('data-panel-entry')).toBe('loading');
    expect(section().getAttribute('data-view-state')).toBe('loading');
    expect(body().textContent).toBe('Loading…');
  });

  it('a_dangling_id_shows_missing', () => {
    mount(['gone'], view([summary('other')]));
    expect(section().getAttribute('data-view-state')).toBe('missing');
    expect(section().getAttribute('data-panel-id')).toBe('gone');
    expect(body().textContent).toBe('Panel gone is missing');
  });

  it('a_removed_entry_shows_missing', () => {
    mount(['a'], view([summary('a', 'Was here', 'removed')]));
    expect(body().textContent).toBe('Panel a is missing');
    expect(tab('a').textContent).toBe('a');
  });

  it('a_skipped_id_shows_its_reason', () => {
    mount(['broken'], view([], [{ panel: 'broken', reason: "panel.json can't be used" }]));
    expect(section().getAttribute('data-view-state')).toBe('skipped');
    expect(body().textContent).toBe("Panel broken isn't loaded: panel.json can't be used");
  });

  it('a_trimmed_panel_shows_the_trim_reason', () => {
    const reason = 'left out to fit one message';
    mount(['big'], view([], [{ panel: 'big', reason }]));
    expect(body().textContent).toBe(`Panel big isn't loaded: ${reason}`);
  });

  it('skipped_entries_without_tabs_change_nothing', () => {
    const plain = mount(['a'], view([summary('a', 'Alpha')]));
    const before = document.body.innerHTML;
    plain.rerender(view([summary('a', 'Alpha')], [{ panel: 'nowhere', reason: 'bad' }]));
    expect(document.body.innerHTML).toBe(before);
  });

  it('shows a list error that is not the loading text', () => {
    mount(['a'], view([summary('a')], [], 'panels/ could not be read'));
    expect(screen.getByText('panels/ could not be read').getAttribute('role')).toBe('alert');
  });

  it('a_view_panel_wins_the_focused_tile_and_a_dangling_one_keeps_the_terminal', async () => {
    const listed = view([summary('a', 'Alpha')]);
    const first = mount(['a'], listed, { focused: 't1', terminal: MOUNT });
    await act(async () => Promise.resolve());
    expect(screen.queryByTestId('terminal-panel')).toBeNull();
    expect(section().getAttribute('data-panel-id')).toBe('a');
    first.rerender(view([summary('other')]));
    await act(async () => Promise.resolve());
    expect(screen.queryByTestId('terminal-panel')).not.toBeNull();
    expect(section()).toBeNull();
    first.rerender({ session: 0, connected: false, list: null });
    await act(async () => Promise.resolve());
    expect(screen.queryByTestId('terminal-panel')).not.toBeNull();
    first.rerender(view([], [{ panel: 'a', reason: 'bad' }]));
    await act(async () => Promise.resolve());
    expect(screen.queryByTestId('terminal-panel')).toBeNull();
    expect(body().textContent).toBe("Panel a isn't loaded: bad");
  });
});
