import { fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { type LayoutSnapshot, parseLayoutEvent } from '../../src/model/layout';
import { TileTree } from '../../src/shell/TileTree';
import { loadLayoutEventFixture } from '../support/memoryLayoutPort';

function fixtureLayout(overrides: Partial<LayoutSnapshot> = {}): LayoutSnapshot {
  const event = parseLayoutEvent(loadLayoutEventFixture().snapshot);
  if (event.kind !== 'snapshot') throw new Error('fixture is a snapshot');
  return { ...event.snapshot, ...overrides };
}

function mount(layout: LayoutSnapshot, notice: string | null = null) {
  const handlers = {
    onFocus: vi.fn(),
    onSelectTab: vi.fn(),
    onSplit: vi.fn(),
    onClose: vi.fn(),
  };
  render(
    <TileTree
      layout={layout}
      notice={notice}
      error={null}
      focusedTile="t1"
      activeTabs={{ t1: 'p1' }}
      {...handlers}
    />,
  );
  return handlers;
}

const tile = (id: string) => within(document.querySelector(`[data-tile-id="${id}"]`) as HTMLElement);

describe('TileTree', () => {
  it('renders the split tree with ratios and the focused tile', () => {
    mount(fixtureLayout({ error: null, dropped: null }));
    const split = document.querySelector('.split') as HTMLElement;
    expect(split.getAttribute('data-dir')).toBe('h');
    const panes = split.querySelectorAll(':scope > .split-pane');
    expect((panes[0] as HTMLElement).style.flex).toBe('0.25 1 0px');
    expect((panes[1] as HTMLElement).style.flex).toBe('0.75 1 0px');
    expect(document.querySelector('[data-tile-id="t1"]')?.getAttribute('aria-current')).toBe('true');
    expect(tile('t1').getByRole('tab', { name: 'p1' }).getAttribute('aria-selected')).toBe('true');
    expect(tile('t1').getByRole('tab', { name: 'p2' }).getAttribute('aria-selected')).toBe('false');
    expect(tile('t2').getByRole('tabpanel').textContent).toBe('No panels');
  });

  it('shows invalid-file error banner', () => {
    mount(fixtureLayout({ dropped: null }));
    expect(screen.getByRole('alert').textContent).toBe('layout.json is invalid');
    expect(document.querySelector('[data-tile-id="t1"]')).not.toBeNull();
  });

  it('shows dropped notice', () => {
    mount(fixtureLayout({ error: null }), "Layout changed elsewhere; your change wasn't applied.");
    const notices = screen.getAllByRole('status').map((node) => node.textContent);
    expect(notices).toEqual(['dropped', "Layout changed elsewhere; your change wasn't applied."]);
  });

  it('split and close send ops', () => {
    const { onSplit, onClose, onFocus, onSelectTab } = mount(fixtureLayout({ error: null, dropped: null }));
    fireEvent.click(tile('t1').getByRole('button', { name: 'Split right' }));
    fireEvent.click(tile('t2').getByRole('button', { name: 'Split down' }));
    fireEvent.click(tile('t2').getByRole('button', { name: 'Close tile' }));
    expect(onSplit.mock.calls).toEqual([
      ['t1', 'h'],
      ['t2', 'v'],
    ]);
    expect(onClose.mock.calls).toEqual([['t2']]);
    fireEvent.click(tile('t1').getByRole('tab', { name: 'p2' }));
    expect(onSelectTab).toHaveBeenCalledWith('t1', 'p2');
    expect(onFocus).toHaveBeenCalledWith('t1');
  });

  it('cannot close the only tile', () => {
    mount(fixtureLayout({ root: { type: 'tile', id: 't1', panels: [], active: null }, error: null }));
    expect((screen.getByRole('button', { name: 'Close tile' }) as HTMLButtonElement).disabled).toBe(true);
  });
});
