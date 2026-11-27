import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { BackendView } from '../../src/model/hosts';
import type { LayoutNode, LayoutSnapshot } from '../../src/model/layout';
import type { DecimalU64, TermId } from '../../src/model/ids';
import { ACTIVE_TERMINAL_TAB, type TerminalTabsContext } from '../../src/model/terminalTabs';
import { TileTree } from '../../src/shell/TileTree';
import { fakeXtermFactory } from '../support/fakeXterm';
import { MemoryPanelPort } from '../support/memoryPanelPort';
import {
  HOST_ID,
  MemoryTerminalPort,
  attachedEvent,
  closedEvent,
  cursor,
  ownershipEvent,
  startedEvent,
} from '../support/memoryTerminalPort';
import { terminal } from '../support/snapshots';
import { FakeResizeObserver, installTerminalDom } from '../support/terminalDom';

const INSTANCE = 'core-a';
const BACKEND: BackendView = { id: 'herdr-core', instance: INSTANCE };
const BUILD = terminal('term-1', 'build', 'make', 'running');
const DEPLOY = terminal('term-2', 'deploy', 'ship', 'idle');
const DONE = terminal('term-3', 'done', 'true', 'exited');
const PIN_1 = `.terminal/${INSTANCE}/term-1`;
const PIN_2 = `.terminal/${INSTANCE}/term-2`;
const PIN_3 = `.terminal/${INSTANCE}/term-3`;

function context(over: Partial<TerminalTabsContext> = {}): TerminalTabsContext {
  const terminals = [BUILD, DEPLOY, DONE];
  return {
    hostId: HOST_ID,
    hostName: 'mini',
    backend: BACKEND,
    terminals: new Map(terminals.map((t) => [t.id, t])),
    spaceTerminals: terminals,
    activeTerminal: BUILD.id,
    ...over,
  };
}

function tile(id: string, panels: string[], active: string | null = panels[0] ?? null): LayoutNode {
  return { type: 'tile', id, panels, active };
}

function layout(a: LayoutNode, b: LayoutNode | null = null): LayoutSnapshot {
  return {
    space: '/work',
    layoutRevision: '1' as LayoutSnapshot['layoutRevision'],
    root: b === null ? a : { type: 'split', dir: 'h', ratio: 0.5, a, b },
    readOnly: false,
    error: null,
    dropped: null,
  };
}

interface Options {
  focused?: string;
  ctx?: TerminalTabsContext | null;
  held?: 'disconnected' | null;
  panels?: boolean;
}

interface Handlers {
  onCloseTab?: (tile: string, panel: string) => void;
  onSelectActiveTerminal?: (terminal: string) => void;
  onRepinTerminal?: (previous: string, terminal: string, tile: string) => void;
}

function tree(
  port: MemoryTerminalPort,
  fake: ReturnType<typeof fakeXtermFactory>,
  snapshot: LayoutSnapshot,
  handlers: Handlers = {},
  { focused = 't1', ctx = context(), held = null, panels = false }: Options = {},
) {
  return (
    <TileTree
      layout={snapshot}
      notice={null}
      error={null}
      focusedTile={focused}
      activeTabs={{}}
      onFocus={vi.fn()}
      onSelectTab={vi.fn()}
      onSplit={vi.fn()}
      onClose={vi.fn()}
      terminalPort={port}
      terminalTabs={ctx}
      held={held}
      createAdapter={fake.factory}
      space={{ hostId: HOST_ID, path: '/work' }}
      {...(panels
        ? {
            panelPort: new MemoryPanelPort(),
            panels: {
              session: 1,
              connected: true,
              list: {
                space: '/work',
                panels: [
                  {
                    id: PIN_1,
                    title: 'A listed lookalike',
                    renderer: 'markdown',
                    revision: '1' as DecimalU64,
                    state: 'validated',
                    error: null,
                    createdBy: null,
                    lifetime: 'persistent',
                  },
                ],
                skipped: [],
                error: null,
              },
            },
          }
        : {})}
      {...handlers}
    />
  );
}

async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

const tabButton = (id: string) => document.querySelector(`[role="tab"][data-panel-id="${id}"]`) as HTMLElement;
const closeButton = (label: string) => screen.getByRole('button', { name: `Close panel ${label}` });
const bodyOf = (id: string) => document.querySelector(`[data-tile-id="${id}"] .tile-body`) as HTMLElement;

beforeEach(() => {
  installTerminalDom();
});

describe('TileTree terminal tabs', () => {
  it('terminal_panels_show_in_unfocused_tiles', async () => {
    const port = new MemoryTerminalPort();
    const fake = fakeXtermFactory();
    render(tree(port, fake, layout(tile('t1', ['p1']), tile('t2', [ACTIVE_TERMINAL_TAB])), {}, { focused: 't1' }));
    await flush();
    const panel = document.querySelector('[data-tile-id="t2"] [data-testid="terminal-panel"]');
    expect(panel).not.toBeNull();
    expect(screen.getAllByTestId('terminal-panel')).toHaveLength(1);
    expect(bodyOf('t2').textContent).toContain('build');
    expect(port.streams).toHaveLength(1);
  });

  it('two_pinned_terminal_tabs_show_side_by_side', async () => {
    const port = new MemoryTerminalPort();
    render(tree(port, fakeXtermFactory(), layout(tile('t1', [PIN_1]), tile('t2', [PIN_2]))));
    await flush();
    expect(screen.getAllByTestId('terminal-panel')).toHaveLength(2);
    expect(bodyOf('t1').textContent).toContain('build');
    expect(bodyOf('t2').textContent).toContain('deploy');
    expect(port.streams).toHaveLength(2);
  });

  it('only the shown tab of a tile is mounted', async () => {
    const port = new MemoryTerminalPort();
    render(tree(port, fakeXtermFactory(), layout(tile('t1', [PIN_1, PIN_2], PIN_1))));
    await flush();
    expect(screen.getAllByTestId('terminal-panel')).toHaveLength(1);
    expect(port.streams).toHaveLength(1);
  });

  it('labels the tabs and marks their entry kinds', async () => {
    const ctx = context({ activeTerminal: DEPLOY.id });
    render(
      tree(
        new MemoryTerminalPort(),
        fakeXtermFactory(),
        layout(tile('t1', [ACTIVE_TERMINAL_TAB, PIN_1, '.terminal/core-a/gone-1', '.terminal/zzz', '.bogus'], '.bogus')),
        {},
        { ctx },
      ),
    );
    await flush();
    const seen = [ACTIVE_TERMINAL_TAB, PIN_1, '.terminal/core-a/gone-1', '.terminal/zzz', '.bogus'].map((id) => [
      tabButton(id).textContent,
      tabButton(id).getAttribute('data-panel-entry'),
    ]);
    expect(seen).toEqual([
      ['Active: deploy', 'active-terminal'],
      ['build', 'terminal'],
      ['gone-1', 'terminal-gone'],
      ['.terminal/zzz', 'missing'],
      ['.bogus', 'missing'],
    ]);
  });

  it('the follow-active tab with no active terminal says to pick one and attaches nothing', async () => {
    const port = new MemoryTerminalPort();
    render(tree(port, fakeXtermFactory(), layout(tile('t1', [ACTIVE_TERMINAL_TAB])), {}, { ctx: context({ activeTerminal: null }) }));
    await flush();
    expect(tabButton(ACTIVE_TERMINAL_TAB).textContent).toBe('Active terminal');
    expect(bodyOf('t1').textContent).toBe('No active terminal. Pick one in the navigator.');
    expect(port.streams).toHaveLength(0);
  });

  it('a_missing_view_tab_shows_missing_not_the_terminal', async () => {
    const port = new MemoryTerminalPort();
    render(
      tree(port, fakeXtermFactory(), layout(tile('t1', ['ghost'])), {}, { focused: 't1', panels: true }),
    );
    await flush();
    expect(screen.queryByTestId('terminal-panel')).toBeNull();
    expect(bodyOf('t1').textContent).toContain('Panel ghost is missing');
    expect(port.streams).toHaveLength(0);
  });

  it('an unknown terminal tab shows missing, and a listed panel cannot take a reserved ID', async () => {
    const port = new MemoryTerminalPort();
    const view = render(
      tree(port, fakeXtermFactory(), layout(tile('t1', ['.bogus'])), {}, { panels: true }),
    );
    await flush();
    expect(bodyOf('t1').textContent).toBe('Panel .bogus is missing');
    expect(tabButton('.bogus').getAttribute('data-panel-entry')).toBe('missing');
    view.rerender(tree(port, fakeXtermFactory(), layout(tile('t1', [PIN_1])), {}, { panels: true }));
    await flush();
    expect(tabButton(PIN_1).getAttribute('data-panel-entry')).toBe('terminal');
    expect(tabButton(PIN_1).textContent).toBe('build');
    expect(screen.getByTestId('terminal-panel')).not.toBeNull();
    expect(document.querySelector('.view-panel')).toBeNull();
  });

  it('a pinned tab before the first terminal list is pending: no attach, × disabled and titled Loading', async () => {
    const port = new MemoryTerminalPort();
    render(
      tree(port, fakeXtermFactory(), layout(tile('t1', [PIN_1])), { onCloseTab: vi.fn() }, { ctx: context({ backend: null }) }),
    );
    await flush();
    expect(bodyOf('t1').textContent).toBe("Waiting for the host's terminal list…");
    expect(tabButton(PIN_1).getAttribute('data-panel-entry')).toBe('terminal-pending');
    const close = closeButton('term-1') as HTMLButtonElement;
    expect(close.disabled).toBe(true);
    expect(close.title).toBe('Loading…');
    expect(screen.queryByRole('button', { name: 'Close' })).toBeNull();
    expect(port.streams).toHaveLength(0);
  });

  it('a_gone_pinned_tab_offers_close_only', async () => {
    const port = new MemoryTerminalPort();
    const onCloseTab = vi.fn();
    const texts: string[] = [];
    const cases: [TerminalTabsContext, string, string][] = [
      [context(), '.terminal/core-a/term-9', 'Terminal term-9 is gone. The host no longer lists it.'],
      [
        context({ backend: { id: 'herdr-core', instance: 'core-b' } }),
        PIN_1,
        'Terminal term-1 is gone. It belonged to a terminal backend this host no longer runs.',
      ],
      [
        context({ backend: { id: 'herdr-core', instance: null } }),
        PIN_1,
        "Terminal term-1 can't be shown: the host doesn't say which terminal backend instance it runs.",
      ],
    ];
    for (const [ctx, tab, text] of cases) {
      const view = render(tree(port, fakeXtermFactory(), layout(tile('t1', [tab])), { onCloseTab }, { ctx }));
      await flush();
      texts.push(screen.getByTestId('gone-terminal-message').textContent ?? '');
      expect(text).toBe(texts.at(-1));
      expect(screen.queryByTestId('terminal-panel')).toBeNull();
      expect(screen.queryByTestId('terminal-switcher')).toBeNull();
      fireEvent.click(within(document.querySelector('[data-testid="gone-terminal"]') as HTMLElement).getByRole('button', { name: 'Close' }));
      view.unmount();
    }
    expect(onCloseTab.mock.calls.map((call) => call[1])).toEqual(['.terminal/core-a/term-9', PIN_1, PIN_1]);
    expect(port.streams).toHaveLength(0);
  });

  it('an_exited_pinned_tab_keeps_its_screen_and_offers_close', async () => {
    const port = new MemoryTerminalPort();
    const onCloseTab = vi.fn();
    render(tree(port, fakeXtermFactory(), layout(tile('t1', [PIN_3])), { onCloseTab }));
    await flush();
    expect(screen.getByTestId('terminal-panel')).not.toBeNull();
    const close = within(screen.getByTestId('terminal-panel')).getByRole('button', { name: 'Close' });
    fireEvent.click(close);
    expect(onCloseTab).toHaveBeenCalledWith('t1', PIN_3);
  });

  it('the follow-active tab offers no Close in its panel', async () => {
    render(tree(new MemoryTerminalPort(), fakeXtermFactory(), layout(tile('t1', [ACTIVE_TERMINAL_TAB])), { onCloseTab: vi.fn() }, { ctx: context({ activeTerminal: DONE.id }) }));
    await flush();
    expect(within(screen.getByTestId('terminal-panel')).queryByRole('button', { name: 'Close' })).toBeNull();
  });

  it('terminal_tab_controls_are_disabled_while_held', async () => {
    const port = new MemoryTerminalPort();
    render(
      tree(port, fakeXtermFactory(), layout(tile('t1', [PIN_3, '.terminal/core-a/term-9'], PIN_3), tile('t2', ['.terminal/core-a/term-9'])), { onCloseTab: vi.fn() }, { held: 'disconnected' }),
    );
    await flush();
    for (const button of screen.getAllByRole('button', { name: /^Close panel / })) {
      expect((button as HTMLButtonElement).disabled).toBe(true);
      expect(button.title).toBe('Host disconnected');
    }
    const panel = screen.getByTestId('terminal-panel');
    const close = within(panel).getByRole('button', { name: 'Close' }) as HTMLButtonElement;
    expect(close.disabled).toBe(true);
    expect(close.title).toBe('Host disconnected');
    const gone = within(bodyOf('t2')).getByRole('button', { name: 'Close' }) as HTMLButtonElement;
    expect(gone.disabled).toBe(true);
    for (const option of within(panel).getAllByRole('option')) {
      expect((option as HTMLOptionElement).disabled).toBe(true);
    }
  });

  it('the follow-active dropdown lists the space terminals and picks without a layout change', async () => {
    const onSelectActiveTerminal = vi.fn();
    render(
      tree(new MemoryTerminalPort(), fakeXtermFactory(), layout(tile('t1', [ACTIVE_TERMINAL_TAB])), { onSelectActiveTerminal }, { held: 'disconnected' }),
    );
    await flush();
    const select = screen.getByTestId('terminal-switcher') as HTMLSelectElement;
    expect([...select.options].map((o) => [o.textContent, o.disabled])).toEqual([
      ['build (running)', false],
      ['deploy (idle)', false],
      ['done (exited)', false],
    ]);
    fireEvent.change(select, { target: { value: 'term-2' } });
    expect(onSelectActiveTerminal).toHaveBeenCalledWith('term-2');
  });

  it('the pinned dropdown re-pins, and disables a terminal already open elsewhere', async () => {
    const onRepinTerminal = vi.fn();
    render(
      tree(new MemoryTerminalPort(), fakeXtermFactory(), layout(tile('t1', [PIN_1]), tile('t2', [PIN_2])), { onRepinTerminal }),
    );
    await flush();
    const select = within(bodyOf('t1')).getByTestId('terminal-switcher') as HTMLSelectElement;
    expect([...select.options].map((o) => [o.textContent, o.disabled])).toEqual([
      ['build (running)', false],
      ['deploy (already open)', true],
      ['done (exited)', false],
    ]);
    fireEvent.change(select, { target: { value: 'term-3' } });
    expect(onRepinTerminal).toHaveBeenCalledWith(PIN_1, PIN_3, 't1');
  });

  it('the pinned dropdown shows a pin of another space as its current option', async () => {
    const onRepinTerminal = vi.fn();
    const ctx = context({ spaceTerminals: [DEPLOY, DONE] });
    render(tree(new MemoryTerminalPort(), fakeXtermFactory(), layout(tile('t1', [PIN_1])), { onRepinTerminal }, { ctx }));
    await flush();
    const select = within(bodyOf('t1')).getByTestId('terminal-switcher') as HTMLSelectElement;
    expect(select.value).toBe('term-1');
    expect([...select.options].map((o) => [o.value, o.textContent, o.disabled])).toEqual([
      ['term-1', 'build (running)', false],
      ['term-2', 'deploy (idle)', false],
      ['term-3', 'done (exited)', false],
    ]);
    expect(select.selectedOptions[0]?.value).toBe('term-1');
    fireEvent.change(select, { target: { value: 'term-2' } });
    expect(onRepinTerminal).toHaveBeenCalledWith(PIN_1, PIN_2, 't1');
  });

  it('the follow-active dropdown shows an active terminal of another space as its current option', async () => {
    const ctx = context({ spaceTerminals: [DEPLOY] });
    render(tree(new MemoryTerminalPort(), fakeXtermFactory(), layout(tile('t1', [ACTIVE_TERMINAL_TAB])), {}, { ctx }));
    await flush();
    const select = screen.getByTestId('terminal-switcher') as HTMLSelectElement;
    expect(select.value).toBe('term-1');
    expect([...select.options].map((o) => o.value)).toEqual(['term-1', 'term-2']);
  });

  it('a terminal whose ID is too long to pin is disabled with its own reason, not as already open', async () => {
    const long = terminal('x'.repeat(300), 'long', 'sh', 'idle');
    const ctx = context({ spaceTerminals: [BUILD, long] });
    render(tree(new MemoryTerminalPort(), fakeXtermFactory(), layout(tile('t1', [PIN_1])), {}, { ctx }));
    await flush();
    const select = within(bodyOf('t1')).getByTestId('terminal-switcher') as HTMLSelectElement;
    const option = [...select.options].find((o) => o.value === long.id) as HTMLOptionElement;
    expect(option.textContent).toBe("long (can't be pinned)");
    expect(option.disabled).toBe(true);
    expect(option.title).toBe("This terminal can't be pinned: its ID is invalid or too long");
    expect(option.title).not.toContain('Already open');
  });

  it('shows the backend id in the header', async () => {
    render(tree(new MemoryTerminalPort(), fakeXtermFactory(), layout(tile('t1', [PIN_1]))));
    await flush();
    expect(screen.getByTestId('terminal-backend').textContent).toBe('herdr-core');
  });

  it('only the focused tile claims an unowned terminal, and the claim follows the focus', async () => {
    const port = new MemoryTerminalPort();
    const fake = fakeXtermFactory();
    const both = layout(tile('t1', [ACTIVE_TERMINAL_TAB]), tile('t2', [PIN_1]));
    const view = render(tree(port, fake, both, {}, { focused: 't1' }));
    await flush();
    const [one, two] = port.streams;
    if (one === undefined || two === undefined) throw new Error('two streams expected');
    const containers = document.querySelectorAll('.terminal-view');
    expect(containers).toHaveLength(2);
    for (const container of containers) act(() => FakeResizeObserver.resize(container, 800, 340));
    await flush();
    act(() => {
      for (const [stream, attachment] of [[one, 7], [two, 8]] as const) {
        stream.emit(attachedEvent(1, attachment));
        stream.emit(startedEvent(cursor(attachment, 1, 0)));
        stream.emit(ownershipEvent(attachment, 1, null));
      }
    });
    await flush();
    expect(one.commands('requestOwnership')).toEqual([
      { op: 'requestOwnership', attachment: '7', ifVacant: '1' },
    ]);
    expect(two.commands('requestOwnership')).toEqual([]);
    const ops = one.sent.map((command) => command.op);
    expect(ops.indexOf('resize')).toBeGreaterThanOrEqual(0);
    expect(ops.lastIndexOf('resize')).toBeLessThan(ops.indexOf('requestOwnership'));

    view.rerender(tree(port, fake, both, {}, { focused: 't2' }));
    await flush();
    expect(two.commands('requestOwnership')).toEqual([
      { op: 'requestOwnership', attachment: '8', ifVacant: '1' },
    ]);
    expect(one.commands('requestOwnership')).toHaveLength(1);
  });

  it('unmounting a panel stops its controller: detach is sent and its stream closed', async () => {
    const port = new MemoryTerminalPort();
    const fake = fakeXtermFactory();
    const view = render(tree(port, fake, layout(tile('t1', [ACTIVE_TERMINAL_TAB]))));
    await flush();
    const container = document.querySelector('.terminal-view');
    if (container === null) throw new Error('no terminal view');
    act(() => FakeResizeObserver.resize(container, 800, 340));
    await flush();
    const stream = port.streams[0];
    if (stream === undefined) throw new Error('stream not open');
    act(() => {
      stream.emit(attachedEvent(1, 7));
      stream.emit(startedEvent(cursor(7, 1, 0)));
      stream.emit(ownershipEvent(7, 1, 8));
    });
    await flush();
    expect(stream.commands('detach')).toHaveLength(0);
    view.rerender(tree(port, fake, layout(tile('t1', [ACTIVE_TERMINAL_TAB])), {}, { ctx: context({ activeTerminal: null }) }));
    await flush();
    act(() => stream.emit(closedEvent(cursor(7, 1, 1), 'detached')));
    await flush();
    expect(screen.queryByTestId('terminal-panel')).toBeNull();
    expect(stream.commands('detach')).toEqual([{ op: 'detach', attachment: '7' }]);
    expect(stream.commands('close')).toHaveLength(1);
    expect(fake.instances[0]?.disposed).toBe(true);
  });

  it('switching the active terminal remounts the panel: term-1 is detached and term-2 attached', async () => {
    const port = new MemoryTerminalPort();
    const fake = fakeXtermFactory();
    const one = layout(tile('t1', [ACTIVE_TERMINAL_TAB]));
    const view = render(tree(port, fake, one));
    await flush();
    const resize = () => {
      const container = document.querySelector('.terminal-view');
      if (container === null) throw new Error('no terminal view');
      act(() => FakeResizeObserver.resize(container, 800, 340));
    };
    resize();
    await flush();
    const first = port.streams[0];
    if (first === undefined) throw new Error('stream not open');
    act(() => {
      first.emit(attachedEvent(1, 7));
      first.emit(startedEvent(cursor(7, 1, 0)));
      first.emit(ownershipEvent(7, 1, 8));
    });
    await flush();
    view.rerender(tree(port, fake, one, {}, { ctx: context({ activeTerminal: DEPLOY.id }) }));
    await flush();
    act(() => first.emit(closedEvent(cursor(7, 1, 1), 'detached')));
    await flush();
    resize();
    await flush();
    expect(first.commands('detach')).toEqual([{ op: 'detach', attachment: '7' }]);
    expect(port.streams).toHaveLength(2);
    expect(
      port.streams[1]?.commands('attach').flatMap((command) => (command.op === 'attach' ? [command.terminal] : [])),
    ).toEqual(['term-2' as TermId]);
    expect(screen.getAllByTestId('terminal-panel')).toHaveLength(1);
  });
});
