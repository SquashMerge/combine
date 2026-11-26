import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { dictWith, emptyDict } from '../../src/model/dict';
import { type LayoutSnapshot, parseLayoutEvent } from '../../src/model/layout';
import type { DecimalU64 } from '../../src/model/ids';
import type { PanelSummary } from '../../src/model/panels';
import { HELD_NOTICE, HELD_TITLE, type HeldReason } from '../../src/shell/heldLayout';
import { TileTree } from '../../src/shell/TileTree';
import { MemoryPanelPort } from '../support/memoryPanelPort';
import { loadLayoutEventFixture } from '../support/memoryLayoutPort';
import { MemoryTerminalPort } from '../support/memoryTerminalPort';

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
    terminalPort: new MemoryTerminalPort(),
    terminal: null,
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

describe('TileTree with tile IDs that name inherited properties', () => {
  const handlers = () => ({
    onFocus: vi.fn(),
    onSelectTab: vi.fn(),
    onSplit: vi.fn(),
    onClose: vi.fn(),
    terminalPort: new MemoryTerminalPort(),
    terminal: null,
  });

  it.each(['__proto__', 'constructor', 'toString', 'hasOwnProperty'])(
    'renders tile %s with no active panel when no tab is set',
    (id) => {
      const props = handlers();
      render(
        <TileTree
          layout={fixtureLayout({ root: { type: 'tile', id, panels: [], active: null }, error: null, dropped: null })}
          notice={null}
          error={null}
          focusedTile={id}
          activeTabs={{}}
          {...props}
        />,
      );
      expect(tile(id).getByRole('tabpanel').textContent).toBe('No panels');
      expect(tile(id).queryAllByRole('tab')).toEqual([]);
      expect(document.querySelector(`[data-tile-id="${id}"]`)?.getAttribute('aria-current')).toBe('true');
      fireEvent.click(tile(id).getByRole('button', { name: 'Split right' }));
      expect(props.onSplit).toHaveBeenCalledWith(id, 'h');
    },
  );

  it('uses an own active tab stored under __proto__', () => {
    render(
      <TileTree
        layout={fixtureLayout({
          root: { type: 'tile', id: '__proto__', panels: ['p1', 'p2'], active: 'p1' },
          error: null,
          dropped: null,
        })}
        notice={null}
        error={null}
        focusedTile={null}
        activeTabs={dictWith(emptyDict<string>(), '__proto__', 'p2')}
        {...handlers()}
      />,
    );
    expect(tile('__proto__').getByRole('tab', { name: 'p2' }).getAttribute('aria-selected')).toBe('true');
    expect(tile('__proto__').getByRole('tabpanel').textContent).toBe('p2');
  });
});

describe('TileTree panel controls', () => {
  const summary = (id: string, lifetime: PanelSummary['lifetime']): PanelSummary => ({
    id,
    title: null,
    renderer: 'markdown',
    revision: '1' as DecimalU64,
    state: 'validated',
    error: null,
    createdBy: null,
    lifetime,
  });

  function mountPanels(options: { held?: HeldReason | null; actionError?: string | null; busy?: string[] } = {}) {
    const handlers = { onCloseTab: vi.fn(), onPinPanel: vi.fn() };
    const onSplit = vi.fn();
    const onClose = vi.fn();
    render(
      <TileTree
        layout={fixtureLayout({ error: null, dropped: null })}
        notice={null}
        error={null}
        focusedTile="t1"
        activeTabs={{ t1: 'p1' }}
        onFocus={vi.fn()}
        onSelectTab={vi.fn()}
        onSplit={onSplit}
        onClose={onClose}
        terminalPort={new MemoryTerminalPort()}
        terminal={null}
        space={{ hostId: 'h' as never, path: '/work' }}
        panelPort={new MemoryPanelPort()}
        panels={{
          session: 1,
          connected: true,
          list: {
            space: '/work',
            panels: [summary('p1', 'scratch'), summary('p2', 'persistent')],
            skipped: [],
            error: null,
          },
        }}
        held={options.held ?? null}
        actionError={options.actionError ?? null}
        busyPanels={new Set(options.busy ?? [])}
        {...handlers}
      />,
    );
    return { ...handlers, onSplit, onClose };
  }

  const control = (name: string) => screen.getByRole('button', { name }) as HTMLButtonElement;

  it('each listed tab has a close control and only scratch tabs a pin control', () => {
    const { onCloseTab, onPinPanel } = mountPanels();
    expect(document.querySelectorAll('.tab-close')).toHaveLength(2);
    expect(document.querySelectorAll('.tab-pin')).toHaveLength(1);
    expect(screen.queryByRole('button', { name: 'Pin panel p2' })).toBeNull();
    expect(control('Close panel p1').title).toBe('Close panel');
    expect(control('Pin panel p1').title).toBe('Keep it in the project');
    fireEvent.click(control('Close panel p2'));
    fireEvent.click(control('Pin panel p1'));
    expect(onCloseTab).toHaveBeenCalledWith('t1', 'p2');
    expect(onPinPanel).toHaveBeenCalledWith('p1');
  });

  it('disables a tab control while its panel is busy', () => {
    mountPanels({ busy: ['p1'] });
    expect(control('Close panel p1').disabled).toBe(true);
    expect(control('Close panel p1').title).toBe('Working…');
    expect(control('Pin panel p1').disabled).toBe(true);
    expect(control('Close panel p2').disabled).toBe(false);
  });

  it('shows an action error as an alert', () => {
    mountPanels({ actionError: "Couldn't close p1: no" });
    expect(screen.getByRole('alert').textContent).toBe("Couldn't close p1: no");
    expect(document.querySelector('.panel-action-error')).not.toBeNull();
  });

  it.each(['disconnected', 'reconnecting', 'failed'] as const)(
    'a held tree disables every edit control with its reason (%s)',
    (reason) => {
      const { onSplit } = mountPanels({ held: reason });
      for (const name of ['Close panel p1', 'Close panel p2', 'Pin panel p1']) {
        expect(control(name).disabled).toBe(true);
        expect(control(name).title).toBe(HELD_TITLE[reason]);
      }
      for (const button of screen.getAllByRole('button', { name: /^(Split right|Split down|Close tile)$/ })) {
        expect((button as HTMLButtonElement).disabled).toBe(true);
        expect(button.title).toBe(HELD_TITLE[reason]);
      }
      fireEvent.click(screen.getAllByRole('button', { name: 'Split right' })[0] as HTMLElement);
      expect(onSplit).not.toHaveBeenCalled();
      expect(document.querySelector('main')?.getAttribute('data-held')).toBe(reason);
      expect(document.querySelector('.layout-held')?.textContent).toBe(HELD_NOTICE[reason]);
      expect(tile('t1').getByRole('tab', { name: 'p2' }).hasAttribute('disabled')).toBe(false);
    },
  );

  it('a live tree has no held mark or banner', () => {
    mountPanels();
    expect(document.querySelector('main')?.hasAttribute('data-held')).toBe(false);
    expect(document.querySelector('.layout-held')).toBeNull();
    expect(control('Close panel p1').disabled).toBe(false);
  });
});

describe('tile bar containment (shell.css)', () => {
  const css = readFileSync(join(import.meta.dirname, '../../src/shell/shell.css'), 'utf8');

  const declarations = (selector: string): string[] => {
    const rule = css
      .split('}')
      .map((block) => block.split('{'))
      .find(([head]) => head?.trim() === selector);
    expect(rule, `shell.css has a rule for ${selector}`).toBeDefined();
    return (rule?.[1] ?? '').split(';').map((line) => line.trim().replace(/\s+/g, ' '));
  };

  it('clips a tile to its own width so its bar cannot spill into a neighbour', () => {
    expect(declarations('.tile')).toEqual(
      expect.arrayContaining(['min-width: 0', 'overflow: hidden']),
    );
  });

  it('wraps the bar and its tablist instead of overflowing the tile', () => {
    expect(declarations('.tile-bar')).toContain('flex-wrap: wrap');
    expect(declarations('.tile-bar [role="tablist"]')).toEqual(
      expect.arrayContaining(['flex-wrap: wrap', 'min-width: 0']),
    );
  });

  it('keeps tile buttons and tab controls at their natural size', () => {
    expect(declarations('.tile-bar > button,\n.tile-bar .tab-wrap')).toEqual(
      expect.arrayContaining(['flex: none', 'white-space: nowrap']),
    );
  });
});
